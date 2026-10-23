//! Key-frame tile decoders: the per-tile walks with loop restoration, the multi-tile driver and the CDF-carrying key tile. Moved out of `decode/mod.rs` (av1split D10); the items are unchanged apart from `pub(super)` visibility.

use super::*;

/// [`decode_key_frame_tile`] with this frame header's own `lr_params` (spec
/// 5.9.20) instead of the no-restoration default -- the per-unit filters
/// themselves live in the tile payload, so the header is all this needs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_key_frame_tile_lr(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    enable_filter_intra: bool,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    tx_select: bool,
    reduced_tx_set: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    lr: &LoopRestorationParams,
    fctx: &crate::decode::FrameCtx,
) -> Result<Picture> {
    let single_tile = TileInfo {
        mi_col_starts: vec![0, mi_cols],
        mi_row_starts: vec![0, mi_rows],
        ..TileInfo::default()
    };
    decode_key_frame_tiles_lr(
        &[data],
        &single_tile,
        mi_cols,
        mi_rows,
        base_q_idx,
        frame_width,
        frame_height,
        enable_filter_intra,
        cdef,
        loop_filter,
        tx_select,
        reduced_tx_set,
        allow_screen_content_tools,
        allow_intrabc,
        lr,
        fctx,
    )
}

/// [`decode_key_frame_tile_lr`] for a frame coded as SEVERAL tiles: the
/// encoder's own filter search hands back one payload per tile plus the
/// `tile_info` its header carries, and the per-tile CDF/LR/neighbour resets
/// are the decoder's own (`decode_key_frame_tile_with_cdfs`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_key_frame_tiles_lr(
    tiles: &[&[u8]],
    tile_info: &TileInfo,
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    enable_filter_intra: bool,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    tx_select: bool,
    reduced_tx_set: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    lr: &LoopRestorationParams,
    fctx: &crate::decode::FrameCtx,
) -> Result<Picture> {
    let delta = DeltaParams::default();
    decode_key_frame_tile_with_cdfs(
        tiles,
        tile_info,
        mi_cols,
        mi_rows,
        base_q_idx,
        crate::quant::QuantDeltas::default(),
        frame_width,
        frame_height,
        enable_filter_intra,
        false,
        cdef,
        loop_filter,
        lr,
        None,
        tx_select,
        reduced_tx_set,
        allow_screen_content_tools,
        allow_intrabc,
        delta,
        fctx,
    )
    .map(|(picture, _)| picture)
}

/// lane-superres r3: the real decoded margin beyond `frame_width` up to
/// the mi-aligned `true_width`, set by the most recent
/// [`decode_key_frame_tile_with_cdfs`] OR (lane-superres r10)
/// [`decode_inter_frame_tile_with_cdfs`] call -- both frame kinds crop
/// their reconstructed buffer down from the same mi-aligned extent, so
/// they share this one slot. See each function's own comment at its
/// write site. `None` when there was no margin to save.
/// The margin the most recent key- or inter-frame tile decode stashed
/// (see [`LAST_FRAME_WIDE_MARGIN`]) -- `stream.rs`'s superres path
/// reads this immediately after the call, before any other frame
/// decode can overwrite it.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn take_last_frame_wide_margin(fctx: &crate::decode::FrameCtx) -> Option<Picture> {
    fctx.last_frame_wide_margin.with(|m| m.borrow_mut().take())
}

/// [`decode_key_frame_tile`], threading a cross-frame CDF forward (spec
/// 7.20's `load_cdfs`): `initial_cdfs` is the caller's forwarded state (from
/// a prior frame's saved reference slot) when set, or the spec 8.4 defaults
/// when `None` (the only case [`decode_key_frame_tile`] itself ever needs,
/// since a key frame's own header always forces `primary_ref_frame ==
/// PRIMARY_REF_NONE`). Returns the tile's own end-of-tile adapted table
/// alongside the picture, for the caller to save into whichever reference
/// slots this frame's `refresh_frame_flags` names.
/// The order the 64x64 superblock quadrants of a tile are decoded in.
///
/// At `sb_size == 64` this is plain raster order over the tile's own
/// superblock grid. At `sb_size == 128` libaom's `decode_partition` recurses
/// each 128x128 superblock in raster order and visits its four 64x64
/// quadrants (TL, TR, BL, BR) before moving to the next superblock, so the
/// 64-level bodies must run in that nested order. `row_start` marks the first
/// item of each *superblock* row (where the left neighbour context resets),
/// `sb_start` the first item of each superblock (where the 128 root's own
/// loop-restoration units and partition symbol are read).
pub(super) fn sb_visit_order(
    r0: u32,
    r1: u32,
    c0: u32,
    c1: u32,
    sb128: bool,
) -> Vec<(u32, u32, bool, bool)> {
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

thread_local! {
    /// lane-av1muchunk422b: how many TX_32X32 chroma units the **compound**
    /// `side > 64` mu-chunk walk allocated (`decode_inter_block`'s compound
    /// arm, `mu_units(.., chroma_side * chroma_side)`). This is the walk
    /// `lanes/av1muchunk422.report.md` called site 2 and could not reach at
    /// 4:2:2; `CHROMA_SPLIT_TX_HITS` fires at all seventeen unit sites and
    /// cannot attribute a bump to this arm. The single-reference twin (that
    /// report's site 5) shares this exact source text, so the bump is routed
    /// through `is_compound` and only this counter names site 2.
    ///
    /// Counted at the ALLOCATION, not at the `side > 64` guard: the arm's
    /// lossless branch returns before the unit loop, so `W_intrabc.obu`
    /// reaches the guard at ss (1,0) four times and allocates nothing -- a
    /// guard-level counter would call that stream a witness of this walk.
    pub(crate) static MU_CHUNK_COMPOUND_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// lane-av1muchunk422b: the intra-in-inter twin of
    /// [`MU_CHUNK_COMPOUND_UNITS`] -- TX_32X32 chroma units allocated by the
    /// intra-in-inter `side > 64` mu-chunk walk (site 8 of the same report).
    /// Same allocation-vs-guard argument: `INTRA_128_IN_INTER_MU_CHROMA_HITS`
    /// is bumped once per mu chunk BEFORE this arm's own lossless branch, so
    /// it is not a witness of the per-unit walk, and `W_intrabc.obu` fires it
    /// eight times at ss (1,0) with zero allocations.
    pub(crate) static MU_CHUNK_INTRA_IN_INTER_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Current value of [`MU_CHUNK_COMPOUND_UNITS`], for a gate's before/after
/// delta.
pub fn mu_chunk_compound_units() -> usize {
    MU_CHUNK_COMPOUND_UNITS.with(std::cell::Cell::get)
}

/// Current value of [`MU_CHUNK_INTRA_IN_INTER_UNITS`], for a gate's
/// before/after delta.
pub fn mu_chunk_intra_in_inter_units() -> usize {
    MU_CHUNK_INTRA_IN_INTER_UNITS.with(std::cell::Cell::get)
}

thread_local! {
    /// lane-av1h5: chunk-major chroma WALKS run for a 128-root plane block --
    /// one per 64x64 mu chunk. libaom codes exactly 4 per unsplit 128x128 root
    /// (`decode_token_recon_block`'s chunk loop), so a stream whose 128 roots
    /// can be counted pins this exactly; the pre-fix redundancy read 64.
    pub(crate) static MU_CHUNK_WALKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1h5: chroma transform units those walks read. On the pinned
    /// 128x96 4:4:4 lossless stream, 1536 per inter frame (768 per plane),
    /// against the oracle's 1536.
    pub(crate) static MU_CHUNK_WALK_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1lm444loss: lossless blocks whose transform-unit grid was
    /// CLIPPED at the frame edge -- `max_block_high`/`max_block_wide` cut the
    /// grid short of the block's own `side / MI`, so libaom codes fewer TX_4X4
    /// units than the block is wide/tall. Zero on any frame whose height and
    /// width are multiples of the superblock, so a gate cannot look armed by
    /// merely decoding.
    pub(crate) static LOSSLESS_EDGE_CLIPPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1lm444loss: the transform units that clip REMOVED -- the
    /// `(side / MI - clipped) * (side / MI)` overflow the pre-fix walk read
    /// past the frame bottom/right edge. On the 128x96 4:4:4 lossless pin this
    /// is 256 per inter frame (8 unclipped luma rows x 32 columns), and 0 on a
    /// frame that fits.
    pub(crate) static LOSSLESS_EDGE_CLIP_UNITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// lane-av1mixllfork: sub-8 (4x8 / 8x4) inter leaves whose var-tx tree
    /// was SUPPRESSED because their own segment is lossless -- libaom's
    /// `parse_decode_block` carries `!xd->lossless[mbmi->segment_id]` as a
    /// conjunct of the tree condition (`decodeframe.c:1237`), so a mixed-
    /// lossless frame codes no `tx_size_cat1` symbol there. Zero on any frame
    /// whose sub-8 leaves all sit in lossy segments, so a gate cannot look
    /// armed by merely decoding.
    ///
    /// CORRECTED 2026-09-30 by the refutation pass (`lanes/refute-av1mixllconj`):
    /// the hit was UNCONDITIONAL inside the else-if, which also serves leaves
    /// whose `tx_select_inter` is off -- the pass measured **33 hits on a build
    /// with the conjunct REMOVED**, i.e. it was counting a population the
    /// conjunct does not touch. It is now guarded on `tx_select_inter`, the same
    /// term the conjunct uses, so a hit means the suppression is the conjunct's
    /// doing.
    pub(crate) static SUB8_LOSSLESS_NO_VARTX: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`MU_CHUNK_WALKS`], for a gate's own before/after delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn mu_chunk_walks() -> usize {
    MU_CHUNK_WALKS.with(std::cell::Cell::get)
}

/// [`MU_CHUNK_WALK_UNITS`], for a gate's own before/after delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn mu_chunk_walk_units() -> usize {
    MU_CHUNK_WALK_UNITS.with(std::cell::Cell::get)
}

/// [`LOSSLESS_EDGE_CLIPPED`] -- lossless blocks whose TX grid the frame-edge
/// clip shortened, for a gate's own before/after delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn lossless_edge_clipped() -> usize {
    LOSSLESS_EDGE_CLIPPED.with(std::cell::Cell::get)
}

/// [`LOSSLESS_EDGE_CLIP_UNITS`] -- transform units the clip removed, i.e. the
/// ones the unclipped pre-fix walk read past the frame edge.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn lossless_edge_clip_units() -> usize {
    LOSSLESS_EDGE_CLIP_UNITS.with(std::cell::Cell::get)
}
/// [`SUB8_LOSSLESS_NO_VARTX`] -- sub-8 inter leaves on a lossless segment that
/// read no var-tx symbol, for a gate's own before/after delta.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn sub8_lossless_no_vartx() -> usize {
    SUB8_LOSSLESS_NO_VARTX.with(std::cell::Cell::get)
}

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_sb128_root(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &Neighbours,
    lr: &LoopRestorationParams,
    lr_grid: &mut crate::restoration::RestorationGrid,
    lr_reference: &mut [(
        crate::restoration::WienerInfo,
        crate::restoration::SgrprojInfo,
    ); 3],
    sb_r: u32,
    sb_c: u32,
    mi_cols: u32,
    mi_rows: u32,
    fctx: &crate::decode::FrameCtx,
) -> Result<usize> {
    let (mi_r128, mi_c128) = ((sb_r & !1) * SB_MI, (sb_c & !1) * SB_MI);
    crate::restoration::read_lr(
        dec,
        cdfs,
        lr,
        lr_grid,
        lr_reference,
        // `read_lr` is the ONE consumer that turns this superblock's
        // mode-info origin into ABSOLUTE frame space -- it divides by the
        // restoration-unit size, which is a real pixel count. The partition
        // decode below is self-consistent with `mi_r128` as written (it
        // only ever uses relative offsets from it), which is why a 128x128
        // stream decoded correctly for 4:2:0 and 4:4:4 while placing its
        // loop-restoration units at double the offset. `sb_r`/`sb_c` index
        // 64px superblocks, so a 128px superblock's origin is half a
        // `SB_MI` per 64px step -- `(sb_r & !1) * SB_MI` is twice the true
        // mi row. The extent is `SB_MI`: a 128px superblock is 16 mi.
        (sb_r & !1) * (SB_MI / 2),
        (sb_c & !1) * (SB_MI / 2),
        SB_MI,
        fctx,
    );
    let at128 = ((mi_r128 / SUB_MI) as usize, (mi_c128 / SUB_MI) as usize);
    let ctx128 = neighbours.partition_ctx(at128, 128);
    let (has_cols128, has_rows128) = (mi_c128 + SB_MI < mi_cols, mi_r128 + SB_MI < mi_rows);
    let part128 = if has_cols128 && has_rows128 {
        hit!(PART128_SYMBOLS);
        let p = dec.symbol(&mut cdfs.partition_w128[ctx128]);
        if crate::envflags::env_flag!("EC_AV1_TRACE") {
            eprintln!("TRACE partition_w128 ctx={ctx128} value={p}");
        }
        p
    } else {
        // lane-sb128 r4: the gathered half-out-of-frame reads DECIDE the
        // partition, they are not a forced split -- exactly the discarded-bit
        // defect the 64 root carries a comment about at `partition_w64`'s
        // gathered arm. Value 1 is SPLIT; 0 is HORZ (rows out of frame) or
        // VERT (cols out of frame), which the refusal below then names
        // instead of silently reconstructing a split.
        hit!(PART128_GATHERED_SYMBOLS);
        match (has_cols128, has_rows128) {
            (true, false) => {
                if dec.symbol_fixed(&gather_of(&cdfs.partition_w128[ctx128], &VERT_ALIKE128)) == 1 {
                    PARTITION_SPLIT
                } else {
                    hit!(PART128_EDGE_HORZ_HITS);
                    PARTITION_HORZ
                }
            }
            (false, true) => {
                if dec.symbol_fixed(&gather_of(&cdfs.partition_w128[ctx128], &HORZ_ALIKE128)) == 1 {
                    PARTITION_SPLIT
                } else {
                    hit!(PART128_EDGE_VERT_HITS);
                    PARTITION_VERT
                }
            }
            _ => {
                PART128_GATHERED_SYMBOLS.with(|c| c.set(c.get() - 1));
                bump_edge_both_cut(3);
                PARTITION_SPLIT
            }
        }
    };
    // lane-sb128b r1: `PARTITION_NONE` -- one 128x128 block -- is decoded by
    // the key-frame caller below. The six remaining arms (HORZ/VERT and the
    // four AB shapes) still need the rect/AB machinery at this size.
    // lane-sb128c r7: HORZ (two 128x64) and VERT (two 64x128) now decode on
    // both tile paths. The four AB shapes (libaom `decode_partition`'s
    // PARTITION_HORZ_A..VERT_B, decodeframe.c) still need the mixed
    // 128x64 + 64x64 + 64x64 visit order and stay refused by name; a 128
    // root has no 1:4 arm at all (`PARTITION_HORZ_4`/`VERT_4` are excluded
    // for BLOCK_128X128 by `av1_get_partition_cdf`'s 8-symbol alphabet,
    // which stops at VERT_B).
    // lane-av1unwritten r4: guard the subsize THIS SYMBOL selected, never the
    // root's own plane block. VERT on a 128x128 root selects BLOCK_64X128,
    // whose 4:2:2 plane block is BLOCK_INVALID; the AB arms select 64x64
    // quadrants (valid), so PARTITION_HORZ / PARTITION_NONE / SPLIT keep
    // decoding untouched.
    refuse_invalid_subsize((128, 128), part128, fctx)?;
    match part128 {
        PARTITION_SPLIT => hit!(PART128_SPLIT_HITS),
        PARTITION_NONE => hit!(PART128_NONE_HITS),
        PARTITION_HORZ => hit!(PART128_HORZ_HITS),
        PARTITION_VERT => hit!(PART128_VERT_HITS),
        // lane-sb128c r9: the four AB shapes at the 128 root -- one
        // 128x64/64x128 half plus two 64x64 quadrants, decoded by both tile
        // paths below in libaom `decode_partition`'s own visit order.
        p if (PARTITION_HORZ_A..=PARTITION_VERT_B).contains(&p) => {
            hit_do! {
                PART128_AB_HITS.with(|c| {
                    let mut h = c.get();
                    h[p - PARTITION_HORZ_A] += 1;
                    c.set(h);
                });
            }
        }
        _ => {
            return Err(unsupported(
                "a 128x128 superblock partition value outside the 8-symbol alphabet",
            ));
        }
    }
    Ok(part128)
}

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_key_frame_tile_with_cdfs(
    tiles: &[&[u8]],
    tile_info: &TileInfo,
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    // lane-sbpart r11: this frame header's own per-plane DC/AC quantizer
    // deltas (spec 5.9.12), threaded to [`QUANT_DELTAS`] at the top of every
    // tile alongside [`CURRENT_Q_IDX`].
    deltas: crate::quant::QuantDeltas,
    frame_width: u32,
    frame_height: u32,
    enable_filter_intra: bool,
    enable_edge_filter: bool,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    lr: &LoopRestorationParams,
    initial_cdfs: Option<Cdfs>,
    tx_select: bool,
    reduced_tx_set: bool,
    allow_screen_content_tools: bool,
    allow_intrabc: bool,
    // lane-realworld r4: this frame header's own `delta` (spec 5.9.17/5.9.18)
    // -- only `q_present`/`q_res` are actually read ([`maybe_read_delta_q`]);
    // `stream.rs` still refuses a frame with `lf_present` set before this
    // function is ever called.
    delta: DeltaParams,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Picture, Cdfs)> {
    // lane-frame80: [`TX_SELECT_INTER`] is set by the INTER tile decoder
    // only, so a key frame decoded after an inter frame on this thread would
    // otherwise inherit it and read its `tx_depth` context off the inter
    // frame's stale `TXFM_CONTEXT` bands.
    fctx.tx_select_inter.with(|c| c.set(false));
    fctx.tx_select_frame.with(|c| c.set(tx_select));
    // lane-av1-rect14: an intrabc block is `is_inter_block`, so its
    // `tx_type` CDF is the INTER set selected by this frame's
    // `reduced_tx_set`. Left at the cell default, the first coded
    // 16x4 intrabc strip reads the wrong alphabet.
    fctx.reduced_tx_set_inter.with(|c| c.set(reduced_tx_set));
    // Every block of an intra-only frame publishes its resolved transform into
    // the `TXFM_CONTEXT` bands exactly like an inter frame's does -- see
    // [`Neighbours::fill_lf_grid_rect`].
    fctx.intra_only.with(|c| c.set(true));
    if mi_cols == 0 || mi_rows == 0 {
        return Err(unsupported("a frame with no mode-info grid"));
    }
    fctx.enable_edge_filter.with(|f| f.set(enable_edge_filter));
    let sb128 = sb128(fctx);
    let (sb_cols, sb_rows) = (mi_cols.div_ceil(SB_MI), mi_rows.div_ceil(SB_MI));
    fctx.intrabc_mi_grid.with(|g| {
        *g.borrow_mut() = allow_intrabc.then(|| {
            (
                crate::mvstack::MiGrid::new(mi_cols as usize, mi_rows as usize),
                mi_cols as usize,
                mi_rows as usize,
            )
        });
    });
    fctx.intrabc_dv.with(|c| c.set(None));
    fctx.cdef_bits.with(|c| c.set(cdef.bits));
    fctx.cdef_sb_cols.with(|c| c.set(sb_cols as usize));
    fctx.cdef_idx_grid
        .with(|g| *g.borrow_mut() = vec![0u8; sb_cols as usize * sb_rows as usize]);
    fctx.delta_q_present.with(|c| c.set(delta.q_present));
    fctx.delta_q_res.with(|c| c.set(1i32 << delta.q_res));
    fctx.delta_lf_present.with(|c| c.set(delta.lf_present));
    fctx.delta_lf_res.with(|c| c.set(1i32 << delta.lf_res));
    fctx.delta_lf_multi.with(|c| c.set(delta.lf_multi));
    let (cols32, rows32) = block_grid(mi_cols, mi_rows);
    let (true_width, true_height) = ((mi_cols * 4) as usize, (mi_rows * 4) as usize);
    let (width, height) = (cols32 as usize * BLOCK, rows32 as usize * BLOCK);

    let mut y = PlaneBuf {
        data: std::borrow::Cow::Owned(fresh_plane(
            width * height,
            true_width,
            true_height,
            width,
            height,
        )),
        width,
        height,
        true_width,
        true_height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width,
        tile_y1: height,
        plane: 0,
    };
    let (cw, ch) = (width >> ss_x(fctx), height >> ss_y(fctx));
    note_frame_chroma_alloc((width, height), (cw, ch), (ss_x(fctx), ss_y(fctx)));
    let (ctw, cth) = (true_width >> ss_x(fctx), true_height >> ss_y(fctx));
    let mut u = PlaneBuf {
        data: std::borrow::Cow::Owned(fresh_plane(cw * ch, ctw, cth, cw, ch)),
        width: cw,
        height: ch,
        true_width: ctw,
        true_height: cth,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: cw,
        tile_y1: ch,
        plane: 1,
    };
    let mut v = PlaneBuf {
        data: std::borrow::Cow::Owned(fresh_plane(cw * ch, ctw, cth, cw, ch)),
        width: cw,
        height: ch,
        true_width: ctw,
        true_height: cth,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: cw,
        tile_y1: ch,
        plane: 2,
    };

    let scan32 = default_scan(TX32);
    let scan16 = default_scan(TX16);
    let scan8 = default_scan(TX8);
    let scan4 = default_scan(TX4);

    let base_cdfs = initial_cdfs.unwrap_or_else(|| Cdfs::new(q_ctx_of(base_q_idx)));
    let mut result_cdfs = base_cdfs.clone();
    let mut neighbours = Neighbours::new(
        cols32 as usize * 2,
        rows32 as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
        fctx,
    );
    // spec 7.16/libaom `av1_alloc_restoration_struct`: the unit grid is
    // sized from `av1_get_upsampled_plane_size`, i.e. the UPSCALED width --
    // loop restoration runs after the superres upscale.
    let mut lr_grid = crate::restoration::RestorationGrid::with_ss(
        lr,
        superres(fctx).map_or(frame_width, |(w, _)| w),
        frame_height,
        fctx.subsampling_x.get(),
        fctx.subsampling_y.get(),
    );

    // lane-tiles r2: each tile gets its own fresh `SymbolDecoder` over its
    // own byte range and its own fresh copy of the frame's initial CDFs
    // (spec 5.11.2 `decode_tile`'s `init_symbol`); only the tile named by
    // `context_update_tile_id` has its end-of-tile adapted table kept as the
    // frame's own output (spec `exit_symbol`) -- every other tile's
    // adaptation is discarded, matching the refusal this replaces.
    for (tile_idx, &tile_bytes) in tiles.iter().enumerate() {
        let tile_num = tile_idx as u32;
        let (trow, tcol) = (tile_num / tile_info.cols, tile_num % tile_info.cols);
        let mi_row0 = tile_info.mi_row_starts[trow as usize];
        let mi_row1 = tile_info.mi_row_starts[trow as usize + 1];
        let mi_col0 = tile_info.mi_col_starts[tcol as usize];
        let mi_col1 = tile_info.mi_col_starts[tcol as usize + 1];
        // Tile boundaries are always superblock-aligned in mi units (spec
        // 5.9.15's uniform/non-uniform derivation both only ever emit
        // multiples of the superblock's own mi size), so plain division is
        // exact here -- no rounding needed.
        // `mi_row0`/`mi_col0` (this tile's own start, `0` or an interior
        // tile boundary) are always superblock-aligned by spec 5.9.15; the
        // frame's own true final edge (`mi_row_starts`/`mi_col_starts`'
        // last entry) is not, so the upper bound needs `div_ceil` the same
        // way the whole-frame `sb_rows`/`sb_cols` above does, to keep the
        // trailing partial superblock row/column in scope.
        let (sb_r0, sb_r1) = (
            (mi_row0 / SB_MI).min(sb_rows),
            mi_row1.div_ceil(SB_MI).min(sb_rows),
        );
        let (sb_c0, sb_c1) = (
            (mi_col0 / SB_MI).min(sb_cols),
            mi_col1.div_ceil(SB_MI).min(sb_cols),
        );

        // lane-t900 r19: spec 5.11.2 `decode_tile` resets `RefLrWiener` /
        // `RefSgrXqd` to the midpoint filter at the top of EVERY tile, not
        // once per frame. Carried across a tile boundary, the loop-restoration
        // coefficients of the next tile decode off the previous tile's
        // reference -- same symbol count, so entropy and reconstruction stay
        // exact and only the filtered pixels of every tile after the first
        // are wrong.
        let mut lr_reference = [(
            crate::restoration::WienerInfo::default(),
            crate::restoration::SgrprojInfo::default(),
        ); 3];
        let mut cdfs = base_cdfs.clone();
        // spec `decode_tile`: `CurrentQIndex` resets to the frame's own
        // `base_q_idx` at the top of every tile, not once per frame.
        fctx.current_q_idx.with(|c| c.set(i32::from(base_q_idx)));
        fctx.quant_deltas.with(|c| c.set(deltas));
        fctx.current_delta_lf.with(|c| c.set([0; 4]));
        if crate::envflags::env_flag!("EC_AV1_TRACE") {
            eprintln!(
                "TRACE key_tile_bytes tile={tile_num} len={} first8={:02x?} base_q_idx={base_q_idx} mi_cols={mi_cols} mi_rows={mi_rows}",
                tile_bytes.len(),
                &tile_bytes[..tile_bytes.len().min(8)]
            );
        }
        let mut dec = SymbolDecoder::new(tile_bytes);
        // lane-av1txr: spec 5.9.2's `disable_cdf_update` -- a frame that sets
        // it codes every symbol against the tables it started with.
        dec.set_cdf_update(!cdf_update_disabled(fctx));
        crate::census::register_cdfs(&cdfs);
        neighbours.start_tile(mi_row0 as usize, mi_col0 as usize, mi_col1 as usize, fctx);
        y.set_tile_origin(
            mi_col0 as usize * 4,
            mi_row0 as usize * 4,
            (mi_col1 as usize * 4).min(y.width),
            (mi_row1 as usize * 4).min(y.height),
        );
        u.set_tile_origin(
            (mi_col0 as usize * 4) >> ss_x(fctx),
            (mi_row0 as usize * 4) >> ss_y(fctx),
            ((mi_col1 as usize * 4) >> ss_x(fctx)).min(u.width),
            ((mi_row1 as usize * 4) >> ss_y(fctx)).min(u.height),
        );
        v.set_tile_origin(
            (mi_col0 as usize * 4) >> ss_x(fctx),
            (mi_row0 as usize * 4) >> ss_y(fctx),
            ((mi_col1 as usize * 4) >> ss_x(fctx)).min(v.width),
            ((mi_row1 as usize * 4) >> ss_y(fctx)).min(v.height),
        );
        hit!(TILE_HITS);

        // lane-sb128b r1: set when the 128 root resolved to `PARTITION_NONE` --
        // its one block covers all four 64x64 quadrants, so the remaining three
        // visits of this superblock code nothing at all.
        let mut sb128_none = false;
        for (sb_r, sb_c, row_start, sb_start) in sb_visit_order(sb_r0, sb_r1, sb_c0, sb_c1, sb128) {
            if row_start {
                neighbours.start_row();
            }
            if sb_start {
                sb128_none = false;
            }
            if sb128_none {
                continue;
            }
            {
                // lane-sb128 r1: the 128 root. Its loop-restoration units are read
                // once for the whole 128x128 (spec 5.11.57 is called with the
                // superblock's own size), then one partition symbol off the
                // `BLOCK_128X128` CDF (8 symbols) decides the root -- only SPLIT
                // recurses into the four 64x64 quadrants this decoder codes.
                if sb128 && sb_start {
                    let part128 = read_sb128_root(
                        &mut dec,
                        &mut cdfs,
                        &neighbours,
                        lr,
                        &mut lr_grid,
                        &mut lr_reference,
                        sb_r,
                        sb_c,
                        mi_cols,
                        mi_rows,
                        fctx,
                    )?;
                    if (PARTITION_HORZ_A..=PARTITION_VERT_B).contains(&part128) {
                        // lane-sb128c r9: the four AB shapes at the 128 root.
                        // libaom `decode_partition` (decodeframe.c) codes each as
                        // two `bsize2` (64x64) squares plus one `subsize`
                        // (128x64 / 64x128) half, visited HORZ_A = TL, TR, bottom
                        // half; HORZ_B = top half, BL, BR; VERT_A = TL, BL, right
                        // half; VERT_B = left half, TR, BR. AB values only come
                        // from the FULL alphabet symbol, which is only read when
                        // both halves of the root are inside the frame, so no
                        // piece needs libaom's `i > 0` frame-edge break (a piece
                        // may still overhang the bottom/right, which
                        // `PlaneBuf::reconstruct`'s r8 clip handles).
                        sb128_none = true;
                        let at128 = ((sb_r & !1) as usize * 4, (sb_c & !1) as usize * 4);
                        let (br, bc) = (at128.0 + 4, at128.1 + 4);
                        let (q_r, q_c) = ((sb_r & !1) as usize, (sb_c & !1) as usize);
                        // Only the VERTICAL arms reorder the squares (TL, BL, TR,
                        // BR), so only they switch libaom's `has_tr_vert_*` /
                        // `has_bl_vert_*` tables (lane-sb128c r6 visit order).
                        let _vert_ab = (part128 == PARTITION_VERT_A || part128 == PARTITION_VERT_B)
                            .then(crate::encode::Reach::vert_ab_partition);
                        // One `cdef_idx` literal per CODED BLOCK, at the first
                        // non-skip one of each 64x64 CDEF unit; the 128x64 /
                        // 64x128 half covers two units and its read lands in the
                        // unit holding its origin, so that value is copied to the
                        // other one (libaom: every mi of the block points at the
                        // same `MB_MODE_INFO`).
                        let cdef_copy = |src: (usize, usize), dst: (usize, usize)| {
                            fctx.cdef_idx_grid.with(|g| {
                                let mut g = g.borrow_mut();
                                let cols = fctx.cdef_sb_cols.with(|c| c.get());
                                if cols == 0 {
                                    return;
                                }
                                let (si, di) = (
                                    (q_r + src.0) * cols + q_c + src.1,
                                    (q_r + dst.0) * cols + q_c + dst.1,
                                );
                                if q_c + src.1 < cols
                                    && q_c + dst.1 < cols
                                    && si < g.len()
                                    && di < g.len()
                                {
                                    g[di] = g[si];
                                }
                            });
                        };
                        macro_rules! square64 {
                            ($at:expr) => {{
                                fctx.cdef_transmitted.with(|c| c.set(false));
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    $at,
                                    SB,
                                    TxbSet::Luma64,
                                    TxbSet::Chroma32,
                                    TX32,
                                    TX32,
                                    (&scan32, &scan32),
                                    false,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    base_q_idx,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                    fctx,
                                )?
                            }};
                        }
                        macro_rules! half128 {
                            ($at:expr, $bw:expr, $bh:expr, $src:expr, $dst:expr) => {{
                                fctx.cdef_transmitted.with(|c| c.set(false));
                                decode_block_128rect(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    $at,
                                    $bw,
                                    $bh,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    base_q_idx,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    tx_select,
                                    reduced_tx_set,
                                    fctx,
                                )?;
                                cdef_copy($src, $dst);
                            }};
                        }
                        match part128 {
                            PARTITION_HORZ_A => {
                                square64!(at128);
                                square64!((at128.0, bc));
                                half128!((br, at128.1), 128, 64, (1, 0), (1, 1));
                            }
                            PARTITION_HORZ_B => {
                                half128!(at128, 128, 64, (0, 0), (0, 1));
                                square64!((br, at128.1));
                                square64!((br, bc));
                            }
                            PARTITION_VERT_A => {
                                square64!(at128);
                                square64!((br, at128.1));
                                half128!((at128.0, bc), 64, 128, (0, 1), (1, 1));
                            }
                            _ => {
                                half128!(at128, 64, 128, (0, 0), (1, 0));
                                square64!((at128.0, bc));
                                square64!((br, bc));
                            }
                        }
                        continue;
                    }
                    if matches!(part128, PARTITION_HORZ | PARTITION_VERT) {
                        // lane-sb128c r8: libaom `decode_partition`
                        // (decodeframe.c:1554) codes PARTITION_HORZ at the 128
                        // root as `subsize = BLOCK_128X64` at (mi_row, mi_col)
                        // plus a second one at (mi_row + hbs, mi_col) only
                        // `if (mi_row + hbs < mi_rows)`; PARTITION_VERT mirrors it
                        // on the column. `hbs` is `mi_size_high[BLOCK_128X128]/2`
                        // = 16 mi. Both halves together cover the whole root, so
                        // the remaining three 64x64 quadrant visits code nothing
                        // (same flag the NONE arm sets).
                        sb128_none = true;
                        let horz = part128 == PARTITION_HORZ;
                        let at128 = ((sb_r & !1) as usize * 4, (sb_c & !1) as usize * 4);
                        let (bw, bh) = if horz { (128usize, 64usize) } else { (64, 128) };
                        for i in 0..2 {
                            let at = if horz {
                                (at128.0 + i * 4, at128.1)
                            } else {
                                (at128.0, at128.1 + i * 4)
                            };
                            let (blk_mi_r, blk_mi_c) = (at.0 * (SUB / MI), at.1 * (SUB / MI));
                            if i > 0
                                && (blk_mi_r >= mi_rows as usize || blk_mi_c >= mi_cols as usize)
                            {
                                break;
                            }
                            // Each half reads its OWN `cdef_idx` literal
                            // (libaom `read_cdef` keys the `-1` test on the
                            // block's own `MB_MODE_INFO`, and the two halves have
                            // different ones).
                            fctx.cdef_transmitted.with(|c| c.set(false));
                            decode_block_128rect(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                at,
                                bw,
                                bh,
                                &mut y,
                                &mut u,
                                &mut v,
                                base_q_idx,
                                enable_filter_intra,
                                allow_screen_content_tools,
                                tx_select,
                                reduced_tx_set,
                                fctx,
                            )?;
                        }
                        // One `cdef_idx` per BLOCK, copied to every 64x64 CDEF
                        // unit it covers (the inter path's own fan-out).
                        let (q_r, q_c) = ((sb_r & !1) as usize, (sb_c & !1) as usize);
                        fctx.cdef_idx_grid.with(|g| {
                            let mut g = g.borrow_mut();
                            let cols = fctx.cdef_sb_cols.with(|c| c.get());
                            if cols == 0 {
                                return;
                            }
                            for (src, dst) in if horz {
                                [((0, 0), (0, 1)), ((1, 0), (1, 1))]
                            } else {
                                [((0, 0), (1, 0)), ((0, 1), (1, 1))]
                            } {
                                let (si, di) = (
                                    (q_r + src.0) * cols + q_c + src.1,
                                    (q_r + dst.0) * cols + q_c + dst.1,
                                );
                                if q_c + src.1 < cols
                                    && q_c + dst.1 < cols
                                    && si < g.len()
                                    && di < g.len()
                                {
                                    g[di] = g[si];
                                }
                            }
                        });
                        continue;
                    }
                    if part128 == PARTITION_NONE {
                        // One 128x128 intra block: four TX_64X64 luma units and a
                        // single 64x64 chroma unit per plane (`av1_get_max_uv_txsize`
                        // of a 128x128 block is TX_64X64 over its 64x64 uv plane
                        // block). No CfL, no filter intra, no palette at this size --
                        // [`read_intra_mode`]'s own size gates already say so.
                        sb128_none = true;
                        fctx.cdef_transmitted.with(|c| c.set(false));
                        let at128 = ((sb_r & !1) as usize * 4, (sb_c & !1) as usize * 4);
                        decode_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            at128,
                            SB * 2,
                            TxbSet::Luma64,
                            TxbSet::Chroma32,
                            TX32,
                            TX32,
                            (scan32, scan32),
                            false,
                            &mut y,
                            &mut u,
                            &mut v,
                            base_q_idx,
                            enable_filter_intra,
                            allow_screen_content_tools,
                            allow_intrabc,
                            scan32,
                            scan16,
                            scan8,
                            scan4,
                            tx_select,
                            reduced_tx_set,
                            fctx,
                        )?;
                        // libaom writes one `cdef_strength` into the block's own
                        // `MB_MODE_INFO`, which every mi cell of the block --
                        // hence all four 64x64 CDEF units -- points at; our grid
                        // is per-unit, so the read (which only ever lands in the
                        // top-left unit, `read_cdef`'s index 0) is copied across.
                        let (q_r, q_c) = ((sb_r & !1) as usize, (sb_c & !1) as usize);
                        fctx.cdef_idx_grid.with(|g| {
                            let mut g = g.borrow_mut();
                            let cols = fctx.cdef_sb_cols.with(|c| c.get());
                            if cols > 0 && q_r * cols + q_c < g.len() {
                                let idx = g[q_r * cols + q_c];
                                for (dr, dc) in [(0, 1), (1, 0), (1, 1)] {
                                    let i = (q_r + dr) * cols + q_c + dc;
                                    if q_c + dc < cols && i < g.len() {
                                        g[i] = idx;
                                    }
                                }
                            }
                        });
                        continue;
                    }
                }
                if !sb128 {
                    crate::restoration::read_lr(
                        &mut dec,
                        &mut cdfs,
                        lr,
                        &mut lr_grid,
                        &mut lr_reference,
                        // True mode-info units: a 64px superblock is 8, and `SB_MI` is 16.
                        // `read_lr` divides by the restoration-unit size (a real pixel count),
                        // so it is the one consumer that needs the absolute origin in real mi.
                        // The half-mi convention these arguments used to carry only worked
                        // while `read_lr` also counted in half-mi -- the two cancelled.
                        sb_r * (SB_MI / 2),
                        sb_c * (SB_MI / 2),
                        SB_MI / 2,
                        fctx,
                    );
                }
                fctx.cdef_transmitted.with(|c| c.set(false));
                let at = (sb_r as usize * 4, sb_c as usize * 4);
                let ctx = neighbours.partition_ctx(at, SB);
                let (has_cols, has_rows) = (
                    sb_c * SB_MI + SB_MI / 2 < mi_cols,
                    sb_r * SB_MI + SB_MI / 2 < mi_rows,
                );
                // Recomputed at this superblock's own half (spec
                // `decode_partition`): a full alphabet symbol when both halves are
                // inside the true frame, a single gathered bit when just one is
                // (the superblock cannot be left whole, so this is forced split),
                // and nothing at all (SPLIT inferred, no bits) when neither is —
                // mirroring [`crate::tile::sb_coeff_key_frame_tile`]'s own
                // three-way write exactly.
                let part = if has_cols && has_rows {
                    if crate::envflags::env_flag!("EC_TRACE") {
                        eprintln!(
                            "EC_PART mi_row={} mi_col={} bsize=12 ctx={} tell={} rng={}",
                            sb_r * SB_MI,
                            sb_c * SB_MI,
                            ctx,
                            dec.debug_bitpos(),
                            dec.debug_state().0
                        );
                    }
                    let p = dec.symbol(&mut cdfs.partition_w64[ctx]);
                    if crate::envflags::env_flag!("EC_TRACE") {
                        eprintln!(
                            "EC_PART_VAL mi_row={} mi_col={} bsize=12 value={}",
                            sb_r * SB_MI,
                            sb_c * SB_MI,
                            p
                        );
                    }
                    if crate::envflags::env_flag!("EC_AV1_TRACE") {
                        eprintln!("TRACE partition_w64 ctx={ctx} value={p}");
                    }
                    p
                } else {
                    // lane-hgkf r1 (class `parsed-then-discarded`, same finding as
                    // the unmerged lane-golomb r1): the gathered bit IS the
                    // partition -- libaom `ec_read_partition_impl` returns
                    // PARTITION_HORZ (resp. VERT) for 0 and only SPLIT for 1.
                    // Forcing SPLIT desynced the last superblock row of every
                    // frame whose height is not a multiple of 64: the ReelB key frames (3840x1608) diverge at mi_row=400 mi_col=0,
                    // the first block of that row.
                    match (has_cols, has_rows) {
                        (true, false) => {
                            trace_edge(
                                (sb_r * SB_MI) as usize,
                                (sb_c * SB_MI) as usize,
                                12,
                                ctx,
                                dec.debug_state().0,
                            );
                            let b = dec.symbol_fixed(&gather(&cdfs.partition_w64[ctx], VERT_ALIKE));
                            bump_edge32(4 + b.min(1));
                            bump_edge_part(b.min(1));
                            if b == 1 {
                                PARTITION_SPLIT
                            } else {
                                PARTITION_HORZ
                            }
                        }
                        (false, true) => {
                            trace_edge(
                                (sb_r * SB_MI) as usize,
                                (sb_c * SB_MI) as usize,
                                12,
                                ctx,
                                dec.debug_state().0,
                            );
                            let b = dec.symbol_fixed(&gather(&cdfs.partition_w64[ctx], HORZ_ALIKE));
                            bump_edge32(4 + b.min(1));
                            bump_edge_part(b.min(1));
                            if b == 1 {
                                PARTITION_SPLIT
                            } else {
                                PARTITION_VERT
                            }
                        }
                        _ => {
                            bump_edge_both_cut(2);
                            PARTITION_SPLIT
                        }
                    }
                };
                // lane-av1unwritten r5: the partition symbol is RESOLVED (a full CDF read or
                // an edge `gather`) and about to be dispatched. `partition_subsize_dims` is
                // libaom's `get_partition_subsize` (common_data.h:71-90) reduced to
                // dimensions, so this is `decode_partition`'s pre-descend check
                // (decodeframe.c:1449-1458) applied to EVERY arm at once -- VERT,
                // VERT_A / VERT_B, HORZ_4 and VERT_4 included. The HORZ siblings keep
                // decoding because the helper is exact, not a heuristic. the 64x64 level binds its symbol to `part`.
                refuse_invalid_subsize((64, 64), part, fctx)?;
                match part {
                    PARTITION_NONE => {
                        decode_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            at,
                            SB,
                            TxbSet::Luma64,
                            TxbSet::Chroma32,
                            TX32,
                            TX32,
                            (scan32, scan32),
                            false,
                            &mut y,
                            &mut u,
                            &mut v,
                            base_q_idx,
                            enable_filter_intra,
                            allow_screen_content_tools,
                            allow_intrabc,
                            scan32,
                            scan16,
                            scan8,
                            scan4,
                            tx_select,
                            reduced_tx_set,
                            fctx,
                        )?;
                    }
                    PARTITION_SPLIT => {
                        for q in 0..4 {
                            let (r32, c32) = (sb_r * 2 + q / 2, sb_c * 2 + q % 2);
                            if r32 >= rows32 || c32 >= cols32 {
                                continue;
                            }
                            let at32 = (r32 as usize * 2, c32 as usize * 2);
                            let ctx32 = neighbours.partition_ctx(at32, BLOCK);
                            let (has_cols32, has_rows32) = (
                                has_half(c32 * BLOCK_MI, BLOCK_MI, mi_cols),
                                has_half(r32 * BLOCK_MI, BLOCK_MI, mi_rows),
                            );
                            let part32 = if has_cols32 && has_rows32 {
                                if crate::envflags::env_flag!("EC_TRACE") {
                                    eprintln!(
                                        "EC_PART mi_row={} mi_col={} bsize=9 ctx={} tell={} rng={}",
                                        r32 * BLOCK_MI,
                                        c32 * BLOCK_MI,
                                        ctx32,
                                        dec.debug_bitpos(),
                                        dec.debug_state().0
                                    );
                                }
                                let p = dec.symbol(&mut cdfs.partition_w32[ctx32]);
                                if crate::envflags::env_flag!("EC_TRACE") {
                                    eprintln!(
                                        "EC_PART_VAL mi_row={} mi_col={} bsize=9 value={}",
                                        r32 * BLOCK_MI,
                                        c32 * BLOCK_MI,
                                        p
                                    );
                                }
                                if crate::envflags::env_flag!("EC_AV1_TRACE") {
                                    let (rng, _) = dec.debug_state();
                                    eprintln!(
                                        "TRACE partition_w32 mi=({},{}) ctx={ctx32} value={p} rng={rng}",
                                        r32 * BLOCK_MI,
                                        c32 * BLOCK_MI
                                    );
                                }
                                p
                            } else {
                                // lane-hgkf r1: the 32-level frame-edge bit is the
                                // partition too (same libaom reader as the
                                // superblock level above).
                                match (has_cols32, has_rows32) {
                                    (true, false) => {
                                        trace_edge(
                                            (r32 * BLOCK_MI) as usize,
                                            (c32 * BLOCK_MI) as usize,
                                            9,
                                            ctx32,
                                            dec.debug_state().0,
                                        );
                                        let b = dec.symbol_fixed(&gather(
                                            &cdfs.partition_w32[ctx32],
                                            VERT_ALIKE,
                                        ));
                                        bump_edge32(b.min(1));
                                        bump_edge_part(2 + b.min(1));
                                        if b == 1 {
                                            PARTITION_SPLIT
                                        } else {
                                            PARTITION_HORZ
                                        }
                                    }
                                    (false, true) => {
                                        trace_edge(
                                            (r32 * BLOCK_MI) as usize,
                                            (c32 * BLOCK_MI) as usize,
                                            9,
                                            ctx32,
                                            dec.debug_state().0,
                                        );
                                        let b = dec.symbol_fixed(&gather(
                                            &cdfs.partition_w32[ctx32],
                                            HORZ_ALIKE,
                                        ));
                                        bump_edge32(b.min(1));
                                        bump_edge_part(2 + b.min(1));
                                        if b == 1 {
                                            PARTITION_SPLIT
                                        } else {
                                            PARTITION_VERT
                                        }
                                    }
                                    _ => {
                                        bump_edge_both_cut(1);
                                        PARTITION_SPLIT
                                    }
                                }
                            };
                            // lane-av1unwritten r4: VERT on a 32x32 block selects
                            // BLOCK_16X32 -- INVALID at 4:2:2. HORZ on the same
                            // block selects BLOCK_32X16, whose (1,0) cell is
                            // BLOCK_16X16 (valid) and MUST keep decoding, so the
                            // check is on the symbol's subsize, not the parent's.
                            refuse_invalid_subsize((32, 32), part32, fctx)?;
                            match part32 {
                                PARTITION_NONE => {
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        at32,
                                        BLOCK,
                                        TxbSet::Luma32,
                                        TxbSet::Chroma16,
                                        TX32,
                                        TX16,
                                        (scan32, scan16),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                }
                                PARTITION_SPLIT => {
                                    for sub in 0..4 {
                                        let (sr, sc) = (
                                            r32 as usize * 2 + sub / 2,
                                            c32 as usize * 2 + sub % 2,
                                        );
                                        if (sr as u32) * SUB_MI >= mi_rows
                                            || (sc as u32) * SUB_MI >= mi_cols
                                        {
                                            continue;
                                        }
                                        let (has_cols16, has_rows16) = (
                                            has_half(sc as u32 * SUB_MI, SUB_MI, mi_cols),
                                            has_half(sr as u32 * SUB_MI, SUB_MI, mi_rows),
                                        );
                                        let at16 = (sr, sc);
                                        let ctx16 = neighbours.partition_ctx(at16, SUB);
                                        if has_cols16 && has_rows16 {
                                            if crate::envflags::env_flag!("EC_TRACE") {
                                                eprintln!(
                                                    "EC_PART mi_row={} mi_col={} bsize=6 ctx={} tell={} rng={}",
                                                    sr * SUB_MI as usize,
                                                    sc * SUB_MI as usize,
                                                    ctx16,
                                                    dec.debug_bitpos(),
                                                    dec.debug_state().0
                                                );
                                            }
                                            let part16 = dec.symbol(&mut cdfs.partition_w16[ctx16]);
                                            // lane-av1unwritten r6: the partition symbol is RESOLVED and about to be
                                            // dispatched. `partition_subsize_dims` is libaom's `get_partition_subsize`
                                            // (common_data.h:71-90) reduced to dimensions, so this is
                                            // `decode_partition`'s pre-descend check (decodeframe.c:1451-1460) on EVERY
                                            // arm -- VERT, VERT_A / VERT_B, HORZ_4 and VERT_4 -- and the HORZ siblings
                                            // keep decoding because the helper is exact, not a heuristic. key-frame 16x16 level.
                                            refuse_invalid_subsize((16, 16), part16, fctx)?;

                                            if crate::envflags::env_flag!("EC_TRACE") {
                                                eprintln!(
                                                    "EC_PART_VAL mi_row={} mi_col={} bsize=6 value={}",
                                                    sr * SUB_MI as usize,
                                                    sc * SUB_MI as usize,
                                                    part16
                                                );
                                            }
                                            if crate::envflags::env_flag!("EC_AV1_TRACE") {
                                                eprintln!(
                                                    "TRACE partition_w16 mi=({},{}) ctx={ctx16} value={part16}",
                                                    sr * SUB_MI as usize,
                                                    sc * SUB_MI as usize
                                                );
                                            }
                                            if part16 == PARTITION_HORZ || part16 == PARTITION_VERT
                                            {
                                                let (bw, bh) = if part16 == PARTITION_HORZ {
                                                    (16, 8)
                                                } else {
                                                    (8, 16)
                                                };
                                                if !chroma_plane_block_codable(
                                                    bw,
                                                    bh,
                                                    ss_x(fctx),
                                                    ss_y(fctx),
                                                ) {
                                                    return Err(refuse_invalid_plane_block(
                                                        bw, bh, fctx,
                                                    ));
                                                }
                                            }
                                            if part16 == PARTITION_NONE {
                                                decode_block(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    SUB,
                                                    TxbSet::Luma16,
                                                    TxbSet::Chroma8,
                                                    TX16,
                                                    TX8,
                                                    (scan16, scan8),
                                                    true,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    base_q_idx,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    allow_intrabc,
                                                    scan32,
                                                    scan16,
                                                    scan8,
                                                    scan4,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?;
                                                continue;
                                            }
                                            if (PARTITION_HORZ_A..=PARTITION_VERT_B)
                                                .contains(&part16)
                                            {
                                                // lane-ab16 r1: the four AB (T-shaped) partitions at the 16x16
                                                // level -- two 8x8 squares plus one 16x8/8x16 strip. libaom's
                                                // `decode_partition` visit order (decodeframe.c):
                                                //   HORZ_A: TL 8x8, TR 8x8, bottom 16x8
                                                //   HORZ_B: top 16x8, BL 8x8, BR 8x8
                                                //   VERT_A: TL 8x8, BL 8x8, right 8x16
                                                //   VERT_B: left 8x16, TR 8x8, BR 8x8
                                                // The squares go through `decode_leaf8`, the strip through
                                                // `decode_leaf_rect` (real, non-skip 16x8/8x16 coefficients --
                                                // lane-rectx r2-r4; the older `decode_block_rect` this VERT_B arm
                                                // used refuses the non-skip case at this size). Both writers keep
                                                // mi-granular mode/skip/palette records, so a later block's
                                                // `above_mi`/`left_mi` sees the real sub-block, not the coarse
                                                // 16x16 cell.
                                                //
                                                // VERT_A/VERT_B visit TL,BL,TR,BR, so the bottom-left square's
                                                // top-right neighbour is the partition's own not-yet-decoded
                                                // rectangle: libaom switches those squares onto `has_tr_vert_*`/
                                                // `has_bl_vert_*` (`reconintra.c` `get_has_tr_table`), which is
                                                // `Reach::vert_ab_partition()` (lane-part32 r5, cherry-picked here).
                                                let arm = part16 - PARTITION_HORZ_A;
                                                bump_ab16(arm);
                                                if part16 == PARTITION_VERT_B {
                                                    hit!(VERT_B_INTRA_HITS);
                                                }
                                                let _vert_guard = if part16 == PARTITION_VERT_A
                                                    || part16 == PARTITION_VERT_B
                                                {
                                                    Some(crate::encode::Reach::vert_ab_partition())
                                                } else {
                                                    None
                                                };
                                                let (mi_row0, mi_col0) =
                                                    (sr * SUB_MI as usize, sc * SUB_MI as usize);
                                                macro_rules! ab_leaf8 {
                                                    ($mi:expr, $prev:expr) => {
                                                        decode_leaf8(
                                                            &mut dec,
                                                            &mut cdfs,
                                                            &mut neighbours,
                                                            at16,
                                                            $mi,
                                                            (&scan8, &scan4),
                                                            $prev,
                                                            &mut y,
                                                            &mut u,
                                                            &mut v,
                                                            base_q_idx,
                                                            enable_filter_intra,
                                                            allow_screen_content_tools,
                                                            allow_intrabc,
                                                            tx_select,
                                                            reduced_tx_set,
                                                            fctx,
                                                        )?
                                                    };
                                                }
                                                macro_rules! ab_strip {
                                                    ($mi:expr, $bw:expr, $bh:expr, $prev:expr) => {
                                                        decode_leaf_rect(
                                                            &mut dec,
                                                            &mut cdfs,
                                                            &mut neighbours,
                                                            at16,
                                                            $mi,
                                                            $bw,
                                                            $bh,
                                                            $prev,
                                                            &mut y,
                                                            &mut u,
                                                            &mut v,
                                                            enable_filter_intra,
                                                            allow_screen_content_tools,
                                                            base_q_idx,
                                                            tx_select,
                                                            reduced_tx_set,
                                                            fctx,
                                                        )?
                                                    };
                                                }
                                                let tl = (mi_row0, mi_col0);
                                                let tr = (mi_row0, mi_col0 + 2);
                                                let bl = (mi_row0 + 2, mi_col0);
                                                let br = (mi_row0 + 2, mi_col0 + 2);
                                                let mut done: Vec<((usize, usize), usize)> =
                                                    Vec::new();
                                                match part16 {
                                                    PARTITION_HORZ_A => {
                                                        let m_tl =
                                                            ab_leaf8!(tl, done.last().copied());
                                                        done.push((tl, m_tl));
                                                        let m_tr =
                                                            ab_leaf8!(tr, done.last().copied());
                                                        done.push((tr, m_tr));
                                                        let m_bot =
                                                            ab_strip!(bl, 16, 8, Some((tl, m_tl)));
                                                        done.push((bl, m_bot));
                                                    }
                                                    PARTITION_HORZ_B => {
                                                        let m_top = ab_strip!(tl, 16, 8, None);
                                                        done.push((tl, m_top));
                                                        let m_bl =
                                                            ab_leaf8!(bl, done.last().copied());
                                                        done.push((bl, m_bl));
                                                        let m_br =
                                                            ab_leaf8!(br, done.last().copied());
                                                        done.push((br, m_br));
                                                    }
                                                    PARTITION_VERT_A => {
                                                        let m_tl =
                                                            ab_leaf8!(tl, done.last().copied());
                                                        done.push((tl, m_tl));
                                                        let m_bl =
                                                            ab_leaf8!(bl, done.last().copied());
                                                        done.push((bl, m_bl));
                                                        let m_right =
                                                            ab_strip!(tr, 8, 16, Some((tl, m_tl)));
                                                        done.push((tr, m_right));
                                                    }
                                                    _ => {
                                                        let m_left = ab_strip!(tl, 8, 16, None);
                                                        done.push((tl, m_left));
                                                        let m_tr =
                                                            ab_leaf8!(tr, done.last().copied());
                                                        done.push((tr, m_tr));
                                                        let m_br =
                                                            ab_leaf8!(br, done.last().copied());
                                                        done.push((br, m_br));
                                                    }
                                                }
                                                // Same coarse-slot write-back the real SPLIT path takes: the
                                                // 16x16 cell's single `above_mode`/`left_mode` entry gets the
                                                // sub-block on the parent's bottom / right edge (the mi-exact map
                                                // above wins wherever it has an entry).
                                                write_back_split_modes(
                                                    &mut neighbours,
                                                    &done,
                                                    (mi_row0, mi_col0),
                                                    (sr, sc),
                                                );
                                                continue;
                                            }
                                            if part16 == PARTITION_HORZ {
                                                hit!(HORZ_VERT_INTRA_HITS);
                                                let (mi_row0, mi_col0) =
                                                    (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                                let top = (mi_row0 as usize, mi_col0 as usize);
                                                let bot = (mi_row0 as usize + 2, mi_col0 as usize);
                                                let mode_top = decode_leaf_rect(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    top,
                                                    16,
                                                    8,
                                                    None,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    base_q_idx,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?;
                                                let mode_bot = decode_leaf_rect(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    bot,
                                                    16,
                                                    8,
                                                    Some((top, mode_top)),
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    base_q_idx,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?;
                                                neighbours.above_mode[sc] = mode_bot;
                                                neighbours.left_mode[sr] = mode_bot;
                                                continue;
                                            }
                                            if part16 == PARTITION_VERT {
                                                hit!(HORZ_VERT_INTRA_HITS);
                                                let (mi_row0, mi_col0) =
                                                    (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                                let left = (mi_row0 as usize, mi_col0 as usize);
                                                let right =
                                                    (mi_row0 as usize, mi_col0 as usize + 2);
                                                let mode_left = decode_leaf_rect(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    left,
                                                    8,
                                                    16,
                                                    None,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    base_q_idx,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?;
                                                let mode_right = decode_leaf_rect(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    right,
                                                    8,
                                                    16,
                                                    Some((left, mode_left)),
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    base_q_idx,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?;
                                                neighbours.above_mode[sc] = mode_right;
                                                neighbours.left_mode[sr] = mode_right;
                                                continue;
                                            }
                                            if part16 == PARTITION_HORZ_4
                                                || part16 == PARTITION_VERT_4
                                            {
                                                // lane-av1unwritten r6: arm census --
                                                // which partition values reach here.
                                                note_subsize_arm(
                                                    part16,
                                                    chroma_plane_block_codable(
                                                        partition_subsize_dims((16, 16), part16).0,
                                                        partition_subsize_dims((16, 16), part16).1,
                                                        ss_x(fctx),
                                                        ss_y(fctx),
                                                    ),
                                                );
                                                decode_rect4_16(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    part16 == PARTITION_HORZ_4,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    base_q_idx,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?;
                                                continue;
                                            }
                                            // lane-r14 r2 (verifier note): every
                                            // one of the 10 symbols this CDF can
                                            // produce is handled above, so SPLIT
                                            // is the only value that reaches
                                            // here -- the refusal that used to
                                            // stand here was unreachable and is
                                            // gone from `refusal_inventory`.
                                            debug_assert_eq!(part16, PARTITION_SPLIT);
                                            // A real (non-straddle) SPLIT of a
                                            // whole 16x16 into four 8x8 leaves --
                                            // the same recursion the straddle
                                            // path below already runs when the
                                            // true frame edge forces it, just
                                            // over all four positions instead of
                                            // the one or two the edge leaves.
                                            let (mi_row0, mi_col0) =
                                                (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                            // Each 8x8 leaf's intra-mode context
                                            // must see its own siblings: the
                                            // bottom-left leaf's ABOVE neighbour
                                            // is leaf 0, not the previously
                                            // decoded leaf 1 (and the bottom-right
                                            // leaf's above is leaf 1, while
                                            // `prev_leaf` only ever carried the
                                            // immediately preceding one). The
                                            // coarse `above_mode`/`left_mode`
                                            // arrays hold one slot per 16x16, so
                                            // the sibling modes are swapped in
                                            // around each leaf call and restored
                                            // after; the loop tail writes the last
                                            // leaf's mode as before.
                                            let mut sib_modes: [Option<(usize, usize)>; 4] =
                                                [None; 4];
                                            for i in 0..4 {
                                                let (mr, mc) =
                                                    (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2);
                                                let leaf_mi = (mr as usize, mc as usize);
                                                let li = i as usize;
                                                let saved_modes = (
                                                    neighbours.above_mode[sc],
                                                    neighbours.left_mode[sr],
                                                );
                                                if li >= 2
                                                    && let Some((below, _)) = sib_modes[li - 2]
                                                {
                                                    neighbours.above_mode[sc] = below;
                                                }
                                                if li % 2 == 1
                                                    && let Some((_, right)) = sib_modes[li - 1]
                                                {
                                                    neighbours.left_mode[sr] = right;
                                                }
                                                let leaf_ctx =
                                                    neighbours.partition_ctx_mi(leaf_mi, 8);
                                                if crate::envflags::env_flag!("EC_TRACE") {
                                                    eprintln!(
                                                        "EC_PART mi_row={} mi_col={} bsize=3 ctx={} tell={} rng={}",
                                                        leaf_mi.0,
                                                        leaf_mi.1,
                                                        leaf_ctx,
                                                        dec.debug_bitpos(),
                                                        dec.debug_state().0
                                                    );
                                                }
                                                let part8 =
                                                    dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                                // lane-av1unwritten r9: the
                                                // SECOND key-frame 8x8 resolution.
                                                // Found by the structural scan, which
                                                // r9 made sensitive to a second
                                                // site at an already covered level.
                                                refuse_invalid_subsize((8, 8), part8, fctx)?;
                                                if crate::envflags::env_flag!("EC_TRACE") {
                                                    eprintln!(
                                                        "EC_PART_VAL mi_row={} mi_col={} bsize=3 value={}",
                                                        leaf_mi.0, leaf_mi.1, part8
                                                    );
                                                }
                                                if crate::envflags::env_flag!("EC_AV1_TRACE") {
                                                    eprintln!(
                                                        "TRACE partition_w8 mi=({mr},{mc}) ctx={leaf_ctx} value={part8}"
                                                    );
                                                }
                                                if part8 == PARTITION_HORZ
                                                    || part8 == PARTITION_VERT
                                                {
                                                    // libaom's `subsize_lookup`: HORZ on an 8x8 is
                                                    // BLOCK_8X4, VERT is BLOCK_4X8.
                                                    let (bw, bh) = if part8 == PARTITION_HORZ {
                                                        (8, 4)
                                                    } else {
                                                        (4, 8)
                                                    };
                                                    if !chroma_plane_block_codable(
                                                        bw,
                                                        bh,
                                                        ss_x(fctx),
                                                        ss_y(fctx),
                                                    ) {
                                                        return Err(refuse_invalid_plane_block(
                                                            bw, bh, fctx,
                                                        ));
                                                    }
                                                }
                                                let leaf_mode = if part8 == PARTITION_NONE {
                                                    decode_leaf8(
                                                        &mut dec,
                                                        &mut cdfs,
                                                        &mut neighbours,
                                                        at16,
                                                        leaf_mi,
                                                        (scan8, scan4),
                                                        None,
                                                        &mut y,
                                                        &mut u,
                                                        &mut v,
                                                        base_q_idx,
                                                        enable_filter_intra,
                                                        allow_screen_content_tools,
                                                        allow_intrabc,
                                                        tx_select,
                                                        reduced_tx_set,
                                                        fctx,
                                                    )
                                                    .map(|m| (m, m))?
                                                } else if part8 == PARTITION_SPLIT {
                                                    decode_leaf_split4(
                                                        &mut dec,
                                                        &mut cdfs,
                                                        &mut neighbours,
                                                        at16,
                                                        leaf_mi,
                                                        scan4,
                                                        None,
                                                        &mut y,
                                                        &mut u,
                                                        &mut v,
                                                        base_q_idx,
                                                        enable_filter_intra,
                                                        allow_intrabc,
                                                        reduced_tx_set,
                                                        fctx,
                                                    )?
                                                } else if part8 == PARTITION_HORZ
                                                    || part8 == PARTITION_VERT
                                                {
                                                    decode_leaf_rect8(
                                                        &mut dec,
                                                        &mut cdfs,
                                                        &mut neighbours,
                                                        at16,
                                                        leaf_mi,
                                                        part8 == PARTITION_VERT,
                                                        scan4,
                                                        &mut y,
                                                        &mut u,
                                                        &mut v,
                                                        base_q_idx,
                                                        enable_filter_intra,
                                                        allow_intrabc,
                                                        tx_select,
                                                        reduced_tx_set,
                                                        fctx,
                                                    )?
                                                } else {
                                                    unreachable!(
                                                        "partition_w8 is a 4-symbol CDF: NONE/HORZ/VERT/SPLIT only"
                                                    );
                                                };
                                                neighbours.above_mode[sc] = saved_modes.0;
                                                neighbours.left_mode[sr] = saved_modes.1;
                                                sib_modes[li] = Some(leaf_mode);
                                            }
                                            if let Some((below, _)) =
                                                sib_modes[2].or(sib_modes[3]).or(sib_modes[0])
                                            {
                                                neighbours.above_mode[sc] = below;
                                            }
                                            if let Some((_, right)) =
                                                sib_modes[1].or(sib_modes[3]).or(sib_modes[0])
                                            {
                                                neighbours.left_mode[sr] = right;
                                            }
                                            continue;
                                        }
                                        // The true edge falls inside this 16x16
                                        // leaf itself (mod-32==8 target sizes,
                                        // lane-av1-rect): one axis only -- an 8x8
                                        // leaf never itself straddles, so the
                                        // block splits cleanly along whichever
                                        // axis is short.
                                        // lane-edgeboth r1: when the true frame
                                        // edge cuts BOTH axes libaom's
                                        // `decode_partition` takes
                                        // `else if (!has_rows && !has_cols)
                                        // partition = PARTITION_SPLIT;` -- no
                                        // symbol of any kind is read, and the four
                                        // 8x8 children are then visited by the
                                        // same loop below, whose
                                        // `mr < mi_rows && mc < mi_cols` filter IS
                                        // libaom's own
                                        // `if (mi_row >= mi_rows || mi_col >= mi_cols) return;`
                                        // early return, so the three out-of-frame
                                        // quadrants get no neighbour or tx-grid
                                        // write.
                                        if !has_cols16 && !has_rows16 {
                                            bump_edge_both_cut(0);
                                        } else {
                                            // lane-golomb r1 (class
                                            // `parsed-then-discarded`): the gathered
                                            // bit IS the partition -- 0 names
                                            // PARTITION_HORZ (resp. VERT), only 1 is
                                            // SPLIT (libaom `ec_read_partition_impl`).
                                            // This path can only walk the four SPLIT
                                            // leaves, so a 0 refuses by name instead
                                            // of desyncing.
                                            trace_edge(
                                                sr * SUB_MI as usize,
                                                sc * SUB_MI as usize,
                                                6,
                                                ctx16,
                                                dec.debug_state().0,
                                            );
                                            let edge_split = if has_cols16 {
                                                dec.symbol_fixed(&gather(
                                                    &cdfs.partition_w16[ctx16],
                                                    VERT_ALIKE,
                                                ))
                                            } else {
                                                dec.symbol_fixed(&gather(
                                                    &cdfs.partition_w16[ctx16],
                                                    HORZ_ALIKE,
                                                ))
                                            };
                                            // lane-hgkf r1: slots 2/3 of
                                            // [`EDGE_PART_HITS`], the gate's proof the
                                            // non-SPLIT edge arm actually decoded.
                                            bump_edge_part(if edge_split == 1 { 3 } else { 2 });
                                            if edge_split == 0 {
                                                // lane-oddh r1: the strip IS decodable
                                                // -- libaom's `decode_partition` codes
                                                // only the FIRST half of an edge
                                                // HORZ/VERT (the second call is
                                                // guarded by the same has_rows/
                                                // has_cols test that made the
                                                // partition non-square), so one
                                                // 16x8 (resp. 8x16) strip covers the
                                                // whole visible band of a frame whose
                                                // height (resp. width) is 8 mod 16 --
                                                // the user's 3840x1608 film.
                                                hit!(HORZ_VERT_INTRA_HITS);
                                                hit!(EDGE_RECT_STRIP_HITS);
                                                // lane-av1subsizesweep: this arm is the ONE
                                                // frame-edge partition read in the file that
                                                // dispatches on its own symbol instead of
                                                // resolving it into a `partN` the level's guard
                                                // covers. The 128-root edge arms
                                                // (decode.rs:35559/35567), the 64-level ones
                                                // (36247/36264) and the 32-level ones
                                                // (36371/36391) all end in `PARTITION_HORZ` /
                                                // `PARTITION_VERT` / `PARTITION_SPLIT`, which
                                                // the guard after their `match` sees -- the
                                                // inter-frame 16-level edge arms (54777/54790)
                                                // resolve into `part16` for the same reason.
                                                // Here `edge_split` goes straight into
                                                // `decode_leaf_rect`, and the shape it carries is
                                                // `BLOCK_8X16` when the COLUMN is short --
                                                // whose (1,0) chroma plane block is
                                                // BLOCK_INVALID (`av1_ss_size_lookup`,
                                                // common_data.c:20), so at 4:2:2 this walks a
                                                // subsize libaom refuses at decodeframe.c:1456.
                                                //
                                                // The check names the LEVEL, not a hand-picked
                                                // shape: the gathered bit resolves to
                                                // PARTITION_HORZ when the columns are full and
                                                // PARTITION_VERT when they are not, and
                                                // `refuse_invalid_subsize` reduces that through
                                                // the same `partition_subsize_dims` every other
                                                // site uses. So the 16x8 strip -- BLOCK_8X16's
                                                // mirror, whose (1,0) cell is BLOCK_8X8 and MUST
                                                // keep decoding -- is untouched, and the guard
                                                // reds on the cell list, not a shape list.
                                                let edge_part = if has_cols16 {
                                                    PARTITION_HORZ
                                                } else {
                                                    PARTITION_VERT
                                                };
                                                refuse_invalid_subsize((16, 16), edge_part, fctx)?;
                                                // lane-av1unwritten r6: arm census -- which
                                                // partition value reaches here.
                                                note_subsize_arm(
                                                    edge_part,
                                                    chroma_plane_block_codable(
                                                        partition_subsize_dims((16, 16), edge_part)
                                                            .0,
                                                        partition_subsize_dims((16, 16), edge_part)
                                                            .1,
                                                        ss_x(fctx),
                                                        ss_y(fctx),
                                                    ),
                                                );
                                                let (mi_row0, mi_col0) =
                                                    (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                                let (bw, bh) =
                                                    if has_cols16 { (16, 8) } else { (8, 16) };
                                                let mode = decode_leaf_rect(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    (mi_row0 as usize, mi_col0 as usize),
                                                    bw,
                                                    bh,
                                                    None,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    base_q_idx,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?;
                                                neighbours.above_mode[sc] = mode;
                                                neighbours.left_mode[sr] = mode;
                                                continue;
                                            }
                                        }
                                        let (mi_row0, mi_col0) =
                                            (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                        let leaf_positions: Vec<(u32, u32)> = (0..4)
                                            .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                            .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                            .collect();
                                        // See the sibling-mode note in the clean
                                        // SPLIT path above.
                                        let mut sib_modes: [Option<(usize, usize)>; 4] = [None; 4];
                                        for (mr, mc) in leaf_positions {
                                            let leaf_mi = (mr as usize, mc as usize);
                                            let li = (((mr - mi_row0) / 2) * 2 + (mc - mi_col0) / 2)
                                                as usize;
                                            let saved_modes = (
                                                neighbours.above_mode[sc],
                                                neighbours.left_mode[sr],
                                            );
                                            if li >= 2
                                                && let Some((below, _)) = sib_modes[li - 2]
                                            {
                                                neighbours.above_mode[sc] = below;
                                            }
                                            if li % 2 == 1
                                                && let Some((_, right)) = sib_modes[li - 1]
                                            {
                                                neighbours.left_mode[sr] = right;
                                            }
                                            let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                            let pre8 = dec.debug_state().0;
                                            let part8 =
                                                dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                            // lane-av1unwritten r9: key-frame 8x8
                                            // level. A VERT here dispatches
                                            // `decode_leaf_rect8` on a raw `part8`
                                            // with no guard anywhere above it --
                                            // `BLOCK_4X8`, which is INVALID at
                                            // 4:2:2 (the shape r5 and r6 each
                                            // flagged). Not corpus-reachable at
                                            // ss(1,0) today, so it is residue rather
                                            // than live pixels, and it is guarded
                                            // anyway.
                                            refuse_invalid_subsize((8, 8), part8, fctx)?;
                                            if crate::envflags::env_flag!("EC_AV1_TRACE") {
                                                eprintln!(
                                                    "TRACE partition_w8 mi=({mr},{mc}) ctx={leaf_ctx} value={part8} pre_rng={pre8}"
                                                );
                                            }
                                            let leaf_mode = if part8 == PARTITION_NONE {
                                                decode_leaf8(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    leaf_mi,
                                                    (scan8, scan4),
                                                    None,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    base_q_idx,
                                                    enable_filter_intra,
                                                    allow_screen_content_tools,
                                                    allow_intrabc,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )
                                                .map(|m| (m, m))?
                                            } else if part8 == PARTITION_SPLIT {
                                                decode_leaf_split4(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    leaf_mi,
                                                    scan4,
                                                    None,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    base_q_idx,
                                                    enable_filter_intra,
                                                    allow_intrabc,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?
                                            } else if part8 == PARTITION_HORZ
                                                || part8 == PARTITION_VERT
                                            {
                                                decode_leaf_rect8(
                                                    &mut dec,
                                                    &mut cdfs,
                                                    &mut neighbours,
                                                    at16,
                                                    leaf_mi,
                                                    part8 == PARTITION_VERT,
                                                    scan4,
                                                    &mut y,
                                                    &mut u,
                                                    &mut v,
                                                    base_q_idx,
                                                    enable_filter_intra,
                                                    allow_intrabc,
                                                    tx_select,
                                                    reduced_tx_set,
                                                    fctx,
                                                )?
                                            } else {
                                                unreachable!(
                                                    "partition_w8 is a 4-symbol CDF: NONE/HORZ/VERT/SPLIT only"
                                                );
                                            };
                                            neighbours.above_mode[sc] = saved_modes.0;
                                            neighbours.left_mode[sr] = saved_modes.1;
                                            sib_modes[li] = Some(leaf_mode);
                                        }
                                        // `record()`'s `above_mode`/`left_mode`
                                        // write is a no-op at an 8x8 leaf's own
                                        // side, so force the write once the whole
                                        // 16x16 slot's leaves are done, from the
                                        // last leaf (mirrors the writer's r15
                                        // fix).
                                        if let Some((below, _)) =
                                            sib_modes[2].or(sib_modes[3]).or(sib_modes[0])
                                        {
                                            neighbours.above_mode[sc] = below;
                                        }
                                        if let Some((_, right)) =
                                            sib_modes[1].or(sib_modes[3]).or(sib_modes[0])
                                        {
                                            neighbours.left_mode[sr] = right;
                                        }
                                    }
                                }
                                PARTITION_HORZ => {
                                    // lane-intradisp r1: two true 32x16 strips.
                                    decode_block_rect(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        at32,
                                        32,
                                        16,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        base_q_idx,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    if !has_rows32 {
                                        bump_edge32(2);
                                    }
                                    // lane-hgkf r1: at the frame edge only the first half of the strip is
                                    // coded (libaom `decode_partition` guards the second half with the same
                                    // test that made this partition non-square).
                                    if has_rows32 {
                                        decode_block_rect(
                                            &mut dec,
                                            &mut cdfs,
                                            &mut neighbours,
                                            (at32.0 + 1, at32.1),
                                            32,
                                            16,
                                            &mut y,
                                            &mut u,
                                            &mut v,
                                            enable_filter_intra,
                                            allow_screen_content_tools,
                                            base_q_idx,
                                            tx_select,
                                            reduced_tx_set,
                                            fctx,
                                        )?;
                                    }
                                }
                                PARTITION_VERT => {
                                    // lane-intradisp r1: mirror of PARTITION_HORZ
                                    // above with width/height swapped.
                                    decode_block_rect(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        at32,
                                        16,
                                        32,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        base_q_idx,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    if !has_cols32 {
                                        bump_edge32(3);
                                    }
                                    // lane-hgkf r1: at the frame edge only the first half of the strip is
                                    // coded (libaom `decode_partition` guards the second half with the same
                                    // test that made this partition non-square).
                                    if has_cols32 {
                                        decode_block_rect(
                                            &mut dec,
                                            &mut cdfs,
                                            &mut neighbours,
                                            (at32.0, at32.1 + 1),
                                            16,
                                            32,
                                            &mut y,
                                            &mut u,
                                            &mut v,
                                            enable_filter_intra,
                                            allow_screen_content_tools,
                                            base_q_idx,
                                            tx_select,
                                            reduced_tx_set,
                                            fctx,
                                        )?;
                                    }
                                }
                                PARTITION_HORZ_A => {
                                    // lane-part32 r1: two 16x16 squares on top +
                                    // a true 32x16 strip below (mirrors
                                    // decode_inter_block's cd6cb6d HORZ_A: TL,
                                    // TR, bottom strip).
                                    hit!(INTRA_HORZ_A_HITS);
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        at32,
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0, at32.1 + 1),
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block_rect(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0 + 1, at32.1),
                                        32,
                                        16,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        base_q_idx,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                }
                                PARTITION_HORZ_B => {
                                    // lane-part32 r1: a true 32x16 strip on top +
                                    // two 16x16 squares below (mirrors HORZ_A
                                    // with the strip/squares order flipped, same
                                    // shape as libaom decode_partition's HORZ_B).
                                    hit!(INTRA_HORZ_B_HITS);
                                    decode_block_rect(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        at32,
                                        32,
                                        16,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        base_q_idx,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0 + 1, at32.1),
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0 + 1, at32.1 + 1),
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                }
                                PARTITION_VERT_A => {
                                    // lane-part32 r1: mirror of HORZ_A with
                                    // width/height swapped (TL, BL, right 16x32
                                    // strip).
                                    hit!(INTRA_VERT_A_HITS);
                                    let _vert_ab = crate::encode::Reach::vert_ab_partition();
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        at32,
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0 + 1, at32.1),
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block_rect(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0, at32.1 + 1),
                                        16,
                                        32,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        base_q_idx,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                }
                                PARTITION_VERT_B => {
                                    // lane-part32 r1: a true 16x32 strip on the
                                    // left + two 16x16 squares on the right
                                    // (libaom decode_partition VERT_B: left
                                    // strip, TR, BR).
                                    hit!(INTRA_VERT_B_HITS);
                                    let _vert_ab = crate::encode::Reach::vert_ab_partition();
                                    decode_block_rect(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        at32,
                                        16,
                                        32,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        base_q_idx,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0, at32.1 + 1),
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                    decode_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        (at32.0 + 1, at32.1 + 1),
                                        SUB,
                                        TxbSet::Luma16,
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        (scan16, scan8),
                                        true,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        base_q_idx,
                                        enable_filter_intra,
                                        allow_screen_content_tools,
                                        allow_intrabc,
                                        scan32,
                                        scan16,
                                        scan8,
                                        scan4,
                                        tx_select,
                                        reduced_tx_set,
                                        fctx,
                                    )?;
                                }
                                PARTITION_HORZ_4 | PARTITION_VERT_4 => {
                                    // lane-tx64x16 r3: the 1:4 pair at the 32x32
                                    // level -- four 32x8 (or 8x32) strips in
                                    // raster order, `quarter_step` = 2 mi, with
                                    // libaom `decode_partition`'s own `i > 0`
                                    // frame-edge break.
                                    let horz = part32 == PARTITION_HORZ_4;
                                    let base_mi = (at32.0 * (SUB / MI), at32.1 * (SUB / MI));
                                    let mut prev: Option<((usize, usize), usize)> = None;
                                    for i in 0..4 {
                                        let strip_mi = if horz {
                                            (base_mi.0 + i * 2, base_mi.1)
                                        } else {
                                            (base_mi.0, base_mi.1 + i * 2)
                                        };
                                        if i > 0
                                            && (strip_mi.0 >= mi_rows as usize
                                                || strip_mi.1 >= mi_cols as usize)
                                        {
                                            break;
                                        }
                                        let (bw, bh) = if horz { (32, 8) } else { (8, 32) };
                                        let (mode, _uv) = decode_block_rect4(
                                            &mut dec,
                                            &mut cdfs,
                                            &mut neighbours,
                                            strip_mi,
                                            bw,
                                            bh,
                                            prev,
                                            &mut y,
                                            &mut u,
                                            &mut v,
                                            enable_filter_intra,
                                            allow_screen_content_tools,
                                            base_q_idx,
                                            tx_select,
                                            fctx,
                                        )?;
                                        prev = Some((strip_mi, mode));
                                    }
                                }
                                _ => {
                                    return Err(unsupported(format!(
                                        "a 32x32 partition type this decoder does not code (value={part32})"
                                    )));
                                }
                            }
                        }
                    }
                    PARTITION_HORZ => {
                        // lane-sbpart r2: two true 64x32 strips.
                        decode_block_rect64(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            at,
                            64,
                            32,
                            &mut y,
                            &mut u,
                            &mut v,
                            enable_filter_intra,
                            allow_screen_content_tools,
                            base_q_idx,
                            tx_select,
                            reduced_tx_set,
                            fctx,
                        )?;
                        if !has_rows {
                            bump_edge32(6);
                        }
                        // lane-golomb r1: at the frame edge only the first half of the
                        // strip is coded (libaom `decode_partition` guards the second
                        // with the same has_rows test that made this partition non-square).
                        if has_rows {
                            decode_block_rect64(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                (at.0 + 2, at.1),
                                64,
                                32,
                                &mut y,
                                &mut u,
                                &mut v,
                                enable_filter_intra,
                                allow_screen_content_tools,
                                base_q_idx,
                                tx_select,
                                reduced_tx_set,
                                fctx,
                            )?;
                        }
                    }
                    PARTITION_VERT => {
                        // lane-sbpart r2: mirror of PARTITION_HORZ above with
                        // width/height swapped.
                        decode_block_rect64(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            at,
                            32,
                            64,
                            &mut y,
                            &mut u,
                            &mut v,
                            enable_filter_intra,
                            allow_screen_content_tools,
                            base_q_idx,
                            tx_select,
                            reduced_tx_set,
                            fctx,
                        )?;
                        if !has_cols {
                            bump_edge32(7);
                        }
                        // lane-golomb r1: at the frame edge only the first half of the
                        // strip is coded (libaom `decode_partition` guards the second
                        // with the same has_cols test that made this partition non-square).
                        if has_cols {
                            decode_block_rect64(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                (at.0, at.1 + 2),
                                32,
                                64,
                                &mut y,
                                &mut u,
                                &mut v,
                                enable_filter_intra,
                                allow_screen_content_tools,
                                base_q_idx,
                                tx_select,
                                reduced_tx_set,
                                fctx,
                            )?;
                        }
                    }
                    PARTITION_HORZ_A | PARTITION_HORZ_B | PARTITION_VERT_A | PARTITION_VERT_B => {
                        // lane-part32 r4: the four superblock-level AB arms, each
                        // two 32x32 squares plus one 64x32/32x64 strip, in
                        // libaom's own `decode_partition` order (decodeframe.c:
                        // HORZ_A = TL, TR, bottom strip; HORZ_B = top strip, BL,
                        // BR; VERT_A = TL, BL, right strip; VERT_B = left strip,
                        // TR, BR). The pieces are exactly the ones already proven
                        // by the `PARTITION_NONE`-under-SPLIT (32x32 square) and
                        // `PARTITION_HORZ`/`VERT` (rect64 strip) arms above.
                        hit_do! {
                            SB_AB_HITS.with(|c| {
                                let mut h = c.get();
                                h[part - PARTITION_HORZ_A] += 1;
                                c.set(h);
                            });
                        }
                        // lane-part32 r5: only the two VERTICAL arms reorder the
                        // square sub-blocks (TL, BL, TR, BR), so only they switch
                        // libaom's `has_tr`/`has_bl` tables.
                        let _vert_ab = (part == PARTITION_VERT_A || part == PARTITION_VERT_B)
                            .then(crate::encode::Reach::vert_ab_partition);
                        macro_rules! square32 {
                            ($at:expr) => {
                                decode_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    $at,
                                    BLOCK,
                                    TxbSet::Luma32,
                                    TxbSet::Chroma16,
                                    TX32,
                                    TX16,
                                    (&scan32, &scan16),
                                    true,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    base_q_idx,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    allow_intrabc,
                                    &scan32,
                                    &scan16,
                                    &scan8,
                                    &scan4,
                                    tx_select,
                                    reduced_tx_set,
                                    fctx,
                                )?
                            };
                        }
                        macro_rules! strip64 {
                            ($at:expr, $bw:expr, $bh:expr) => {
                                decode_block_rect64(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    $at,
                                    $bw,
                                    $bh,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    enable_filter_intra,
                                    allow_screen_content_tools,
                                    base_q_idx,
                                    tx_select,
                                    reduced_tx_set,
                                    fctx,
                                )?
                            };
                        }
                        let (br, bc) = (at.0 + 2, at.1 + 2);
                        match part {
                            PARTITION_HORZ_A => {
                                square32!(at);
                                square32!((at.0, bc));
                                strip64!((br, at.1), 64, 32);
                            }
                            PARTITION_HORZ_B => {
                                strip64!(at, 64, 32);
                                square32!((br, at.1));
                                square32!((br, bc));
                            }
                            PARTITION_VERT_A => {
                                square32!(at);
                                square32!((br, at.1));
                                strip64!((at.0, bc), 32, 64);
                            }
                            _ => {
                                strip64!(at, 32, 64);
                                square32!((at.0, bc));
                                square32!((br, bc));
                            }
                        }
                    }
                    PARTITION_HORZ_4 | PARTITION_VERT_4 => {
                        // lane-tx64x16: the 1:4 pair at 64x64 -- four 64x16 (or
                        // 16x64) strips in raster order. `decode_partition`
                        // (`decodeframe.c`) steps by `quarter_step = mi_size / 4`
                        // (4 mi = 16 px = one SUB unit here) and BREAKS at the
                        // frame edge for `i > 0`, so a partial superblock codes
                        // only the strips whose origin is inside the frame.
                        let horz = part == PARTITION_HORZ_4;
                        for i in 0..4 {
                            let step_mi = i * (SB_MI as usize / 4);
                            let (this_r_mi, this_c_mi) = if horz {
                                (
                                    sb_r as usize * SB_MI as usize + step_mi,
                                    sb_c as usize * SB_MI as usize,
                                )
                            } else {
                                (
                                    sb_r as usize * SB_MI as usize,
                                    sb_c as usize * SB_MI as usize + step_mi,
                                )
                            };
                            if i > 0
                                && (this_r_mi >= mi_rows as usize || this_c_mi >= mi_cols as usize)
                            {
                                break;
                            }
                            let strip_at = if horz {
                                (at.0 + i, at.1)
                            } else {
                                (at.0, at.1 + i)
                            };
                            let (bw, bh) = if horz { (64, 16) } else { (16, 64) };
                            decode_block_rect64(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                strip_at,
                                bw,
                                bh,
                                &mut y,
                                &mut u,
                                &mut v,
                                enable_filter_intra,
                                allow_screen_content_tools,
                                base_q_idx,
                                tx_select,
                                reduced_tx_set,
                                fctx,
                            )?;
                        }
                    }
                    _ => {
                        return Err(unsupported(
                            "a superblock-level partition value outside PARTITION_NONE..PARTITION_VERT_4",
                        ));
                    }
                }
            }
        }
        // lane-timeline: this tile's last symbol is read here.
        crate::timeline::tile_done();

        if tile_num == tile_info.context_update_tile_id {
            result_cdfs = cdfs;
        }
    }
    crate::timeline::recon_done();

    // lane-comppin r4: pre-loop-filter decode-order dump, matching aomdec's
    // own EC_AV1_PREFILT_DUMP shape (decodeframe.c ~5451) -- diffs against
    // that isolate whether a decode-order frame's mismatch already exists
    // in reconstruction (this dump) vs is introduced by the loop filter
    // (EC_AV1_DECODE_ORDER_DUMP's post-filter dump).
    // Decode-order frame marker for the EC_OBMC / EC_TRACE_MODE ladders.
    if crate::envflags::env_flag!("EC_OBMC")
        || crate::envflags::env_flag!("EC_TRACE_MODE")
        || crate::envflags::env_flag!("EC_PRED")
    {
        eprintln!(
            "EC_PICT idx={}",
            PREFILT_PICTURE_IDX.load(std::sync::atomic::Ordering::SeqCst)
        );
    }
    dump_stage16(
        "EC_AV1_PREFILT_DUMP16",
        &y,
        &u,
        &v,
        frame_width as usize,
        frame_height as usize,
        ss_x(fctx),
        ss_y(fctx),
    );
    census_unwritten(&y, &u, &v);
    if let Ok(path) = crate::envflags::var("EC_AV1_PREFILT_DUMP") {
        // lane-av1dumploud: loud on a short write (three discarded
        // `write_all` results before). Expected length = the three planes' own
        // data lengths: this dump is the PADDED plane, not a crop, so the
        // writer's extent is `p.data` itself.
        let idx = PREFILT_PICTURE_IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let narrow = |p: &PlaneBuf<'_>| -> Vec<u8> { p.data.iter().map(|&s| s as u8).collect() };
        crate::dumpio::write_planes(
            "EC_AV1_PREFILT_DUMP",
            format!("{path}.f{idx}"),
            &[&narrow(&y), &narrow(&u), &narrow(&v)],
        );
    } else {
        PREFILT_PICTURE_IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    dump_prefilter_wide(&y, &u, &v);
    // lane-av1fwd: the encoder's filter search re-decodes this same key
    // frame tile per candidate -- capture it once (see [`FilterReplay`]),
    // on the shape whose tail is the two filter calls and nothing else.
    if superres(fctx).is_none()
        && !(0..3).any(|p| {
            lr.frame_restoration_type[p] != ec_av1_syntax::RestorationType::None
                && lr.loop_restoration_size[p] != 0
        })
    {
        capture_filter_replay(
            &y,
            &u,
            &v,
            &neighbours,
            frame_width as usize,
            frame_height as usize,
            width,
            height,
            true_width,
            true_height,
        );
    }
    apply_deblock(
        &mut y,
        &mut u,
        &mut v,
        loop_filter,
        &neighbours,
        frame_width as usize,
        frame_height as usize,
        fctx,
    );
    // lane-perf4: loop restoration is the only reader of the pre-CDEF planes,
    // and a frame that restores nothing copied every plane twice for nothing
    // (three `PlaneBuf` clones here plus the `cdef.to_vec()` inside
    // [`crate::restoration::apply_loop_restoration_plane`], which returns its
    // input unchanged when the plane's type is `None`). Clone only when some
    // plane actually restores.
    let lr_active = (0..3).any(|p| {
        lr.frame_restoration_type[p] != ec_av1_syntax::RestorationType::None
            && lr.loop_restoration_size[p] != 0
    });
    let deblocked = lr_active.then(|| {
        (
            plane_snapshot(&y, 0),
            // lane-av1422filter: the bands are the LR stripe-boundary context
            // (`RESTORATION_UNIT_OFFSET >> ss_y` above/below each 64-luma-row
            // stripe). Chroma stripes are half-height ONLY under 4:2:0; a
            // 4:2:2 frame's chroma (ss_y = 0) stripes on the luma grid, so
            // the hardcoded `1` saved bands at rows 26/60/92/124 where LR
            // reads rows 54..57/118..121 -- every 4:2:2 boundary
            // substitution read uninitialised zeros (frame-0 V (48,0)).
            plane_snapshot(&u, ss_y(fctx) as u32),
            plane_snapshot(&v, ss_y(fctx) as u32),
        )
    });
    // lane-tiny r4: post-deblock / post-CDEF dumps mirroring aomdec's own
    // EC_AV1_POSTDEBLOCK_DUMP (decodeframe.c ~5404) -- with the pre-filter
    // dump above these bisect WHICH filter stage introduced a mismatch.
    dump_stage("EC_AV1_POSTDEBLOCK_DUMP", &y, &u, &v);
    dump_stage16(
        "EC_AV1_POSTDEBLOCK_DUMP16",
        &y,
        &u,
        &v,
        frame_width as usize,
        frame_height as usize,
        ss_x(fctx),
        ss_y(fctx),
    );
    apply_cdef(
        &mut y,
        &mut u,
        &mut v,
        cdef,
        &neighbours,
        (frame_width as usize, frame_height as usize),
        fctx,
        None,
        None,
    );
    dump_stage("EC_AV1_POSTCDEF_DUMP", &y, &u, &v);
    dump_stage16(
        "EC_AV1_POSTCDEF_DUMP16",
        &y,
        &u,
        &v,
        frame_width as usize,
        frame_height as usize,
        ss_x(fctx),
        ss_y(fctx),
    );
    capture_stages(&y, &u, &v, deblocked.as_ref(), fctx);
    // spec 7.16 (libaom `superres_post_decode`, decodeframe.c: called
    // between `av1_cdef_frame` and `av1_loop_restoration_filter_frame`):
    // the horizontal upscale runs HERE -- after CDEF, before loop
    // restoration, not after the whole filter chain. Both the post-CDEF
    // frame and the post-deblock frame that LR substitutes at its stripe
    // boundaries are upscaled (`save_deblock_boundary_lines` runs
    // `av1_upscale_normative_rows` over its saved rows when the frame is
    // scaled), so every LR coordinate from here on is in upscaled space.
    let mut deblocked = deblocked;
    let mut lr_w = frame_width as usize;
    if let Some((upscaled_w, _)) = superres(fctx) {
        let bd = u32::from(bit_depth(fctx));
        let (uw, fw, fh) = (
            upscaled_w as usize,
            frame_width as usize,
            frame_height as usize,
        );
        let up = |p: &PlaneBuf<'_>, in_w: usize, in_h: usize, out_w: usize| PlaneBuf {
            data: std::borrow::Cow::Owned(crate::superres::upscale_plane_strided(
                &p.data,
                p.width,
                in_w,
                in_h,
                p.true_width,
                out_w,
                bd,
            )),
            width: out_w,
            height: in_h,
            true_width: out_w,
            true_height: in_h,
            tile_x0: 0,
            tile_y0: 0,
            tile_x1: out_w,
            tile_y1: in_h,
            plane: p.plane,
        };
        let (cw, ch, cuw) = (
            round_ss(fw, ss_x(fctx)),
            round_ss(fh, ss_y(fctx)),
            round_ss(uw, ss_x(fctx)),
        );
        y = up(&y, fw, fh, uw);
        u = up(&u, cw, ch, cuw);
        v = up(&v, cw, ch, cuw);
        if let Some((dy, du, dv)) = deblocked.take() {
            deblocked = Some((
                up(&dy, fw, fh, uw),
                up(&du, cw, ch, cuw),
                up(&dv, cw, ch, cuw),
            ));
        }
        lr_w = uw;
        crate::superres::note_upscaled_picture();
    }
    if let Some((deblocked_y, deblocked_u, deblocked_v)) = &deblocked {
        apply_loop_restoration(
            &mut y,
            &mut u,
            &mut v,
            deblocked_y,
            deblocked_u,
            deblocked_v,
            lr,
            &lr_grid,
            (lr_w, frame_height as usize),
            fctx,
        );
    }
    if let Some((db_y, db_u, db_v)) = deblocked {
        // LIFO pool: return in reverse so the next frame's `y`/`u`/`v`
        // snapshots pop buffers of their own size and never resize.
        scratch_return(db_v.data.into_owned());
        scratch_return(db_u.data.into_owned());
        scratch_return(db_y.data.into_owned());
    }
    crate::timeline::filters_done();

    let (fw, fh) = (lr_w, frame_height as usize);
    // An upscaled frame is already at its final size with no mi-aligned
    // margin beyond it (the upscale consumed those columns), so the
    // wide-margin stash below has nothing left to hand out.
    let (true_width, true_height) = if superres(fctx).is_some() {
        (fw, fh)
    } else {
        (true_width, true_height)
    };
    if fw == width && fh == height {
        fctx.last_frame_wide_margin.with(|m| *m.borrow_mut() = None);
        return Ok((
            Picture {
                width,
                height,
                y: y.data.into_owned(),
                u: u.data.into_owned(),
                v: v.data.into_owned(),
            },
            result_cdfs,
        ));
    }
    let crop = |plane: &PlaneBuf<'_>, w: usize, h: usize| -> Vec<u16> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend(plane.data[row * plane.width..][..w].iter().copied());
        }
        out
    };
    // lane-superres r3: `true_width`/`true_height` (`mi_cols`/`mi_rows` * 4)
    // is the real reconstructed extent -- columns `[fw, true_width)` hold
    // genuine decoded samples (the last coding block straddles the frame
    // edge whenever `frame_width` isn't 8-sample aligned), not padding.
    // libaom's superres upscale runs on that buffer's own border-extended
    // margin, so it reads those real columns, not a synthetic replicate of
    // column `fw - 1`. Stash them for `stream.rs`'s superres path (a real
    // aomenc 43->64 stream pinned column-by-column against libaom's own
    // `av1_convolve_horiz_rs_c` via `scripts/superres-pin-harness.c` --
    // `row6-realedgeval` proved the replicate-of-`fw-1` padding this
    // decoder used before was off by 1 at the columns the tap window
    // reaches into that real margin; `None` when there is no margin (this
    // frame's `true_width`/`true_height` already equal `fw`/`fh`, e.g. a
    // non-superres frame whose width just isn't 32-block aligned).
    fctx.last_frame_wide_margin.with(|m| {
        *m.borrow_mut() = if true_width > fw || true_height > fh {
            let yc = crop(&y, true_width, true_height);
            if let Ok(path) = crate::envflags::var("EC_AV1_MARGIN_DUMP") {
                static IDX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let idx = IDX.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // corner-cut: this diagnostic dump stays 8-bit-narrowed
                // regardless of the stream's real bit depth (debug-only,
                // never read by a gate); widen if a >8-bit margin dump
                // is ever needed.
                //
                // lane-av1dumploud: loud on a short write. Expected length is
                // `true_width * true_height` -- the crop this dump is exactly,
                // not a re-derived geometry.
                let narrow: Vec<u8> = yc.iter().map(|&s| s as u8).collect();
                crate::dumpio::write_planes(
                    "EC_AV1_MARGIN_DUMP",
                    format!("{path}.f{idx}"),
                    &[&narrow],
                );
            }
            Some(Picture {
                width: true_width,
                height: true_height,
                y: yc,
                u: crop(
                    &u,
                    round_ss(true_width, ss_x(fctx)),
                    round_ss(true_height, ss_y(fctx)),
                ),
                v: crop(
                    &v,
                    round_ss(true_width, ss_x(fctx)),
                    round_ss(true_height, ss_y(fctx)),
                ),
            })
        } else {
            None
        }
    });
    Ok((
        Picture {
            width: fw,
            height: fh,
            y: crop_owned(y, fw, fh),
            u: crop_owned(u, round_ss(fw, ss_x(fctx)), round_ss(fh, ss_y(fctx))),
            v: crop_owned(v, round_ss(fw, ss_x(fctx)), round_ss(fh, ss_y(fctx))),
        },
        result_cdfs,
    ))
}

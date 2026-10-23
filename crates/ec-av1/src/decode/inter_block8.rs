//! Inter-block helpers for the 8x8-leaf and sub-8x8 stamping paths: the grid stamp
//! for a rect sub-8x8 leaf and the 8x8 inter leaf decoder. Moved out of `decode/mod.rs`
//! (av1split D6); the items are unchanged apart from `pub(super)` visibility.

use super::*;

/// Stamps one sub-8x8 inter leaf into every mi cell of its true `w_mi`x`h_mi`
/// footprint (class: a rect block's grid span is its own, never a square guess).
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn grid_stamp_rect(
    grid: &mut MiGrid,
    mi_row: usize,
    mi_col: usize,
    w_mi: usize,
    h_mi: usize,
    ref_frame: i8,
    mv: (i32, i32),
    is_new_mv: bool,
) {
    grid.fill_rect(
        mi_row,
        mi_col,
        h_mi,
        w_mi,
        MiInfo {
            is_inter: true,
            ref_frame,
            ref_frame1: NO_REF1,
            mv1: (0, 0),
            mv: mv16(mv),
            is_new_mv,
            size: w_mi as u8,
            size_h: h_mi as u8,
            is_global_mv0: false,
            is_global_mv1: false,
        },
    );
}

/// Decodes one 8x8 leaf of a straddling 16x16 inter-frame block
/// (lane-av1inter8), mirroring [`crate::tile::write_inter_frame_leaf8`]'s
/// exact symbol order: its own skip flag, intra/inter choice and (when
/// inter) `NEARESTMV`/`NEWMV` chain against this leaf's own 2x2-mi mv-stack
/// window, or (when intra) `Y_MODE`, then its own luma and two chroma
/// transform blocks through [`TxbSet::Luma8Inter`]/[`TxbSet::Chroma4`],
/// same as [`decode_leaf8`]'s coefficient layer but with
/// motion-compensated prediction instead of intra prediction when inter.
/// `outer_at` (in [`SUB`]-grid units) is the enclosing 16x16 slot's
/// skip/intra-inter context, overridden by `prev_leaf` exactly as
/// [`decode_leaf8`] overrides mode context: `Neighbours`' above/left arrays
/// only resolve to [`SUB`] granularity. Hands back this leaf's own skip flag
/// and intra/inter choice, for the next leaf or the caller's final
/// write-back.
#[allow(clippy::too_many_arguments)]
pub(super) fn decode_inter_block8(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    grid: &mut MiGrid,
    mi_cols: u32,
    mi_rows: u32,
    outer_at: (usize, usize),
    leaf_mi: (usize, usize),
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    refpix: &RefPix<'_>,
    base_q_idx: u8,
    scan8: &[u16],
    scan4: &[u16],
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    // lane-gmaffine r1: this frame header's own `global_motion` table (spec
    // 5.9.24), indexed `[ref_frame - LAST_FRAME]` -- same table
    // [`decode_inter_block`] takes, threaded here so the 8x8 leaf can code
    // `GLOBALMV` (`gm_get_motion_vector`), fill its mv stack's missing
    // candidates with the gm mv (gm r6's root cause) and build a global
    // warp prediction.
    global_motion: &[ec_av1_syntax::WarpParams; 7],
    // lane-gmaffine r1: this sequence header's own `enable_dual_filter` bit,
    // for the leaf's own `read_mb_interp_filter` below.
    enable_dual_filter: bool,
    // lane-av1comp: see [`decode_inter_block`]'s own doc -- this leaf path
    // only ever tracks a coarse `LAST_FRAME`-or-intra neighbour shape (no
    // per-leaf `above_ref`/`left_ref` array exists here), so `comp_mode`'s
    // context uses that same approximation.
    reference_select: bool,
    enable_masked_compound: bool,
    // lane-sb128 r4: see [`decode_inter_block`]'s own doc -- `BLOCK_8X8` is
    // always `is_interintra_allowed_bsize`-eligible, so this leaf's own gate
    // drops the `side` check entirely.
    enable_interintra_compound: bool,
    enable_jnt_comp: bool,
    order_hint_bits: u32,
    order_hint: u32,
    ref_order_hints: [u32; 7],
    // lane-av1comp round 14: see [`decode_inter_block`]'s own doc -- this
    // leaf has no per-leaf `above_skip_mode`/`left_skip_mode` array either,
    // so its `skip_mode` context reuses the same [`SUB`]-grid
    // `above_skip_mode[cmi]`/`left_skip_mode[rmi]` the enclosing 16x16 slot
    // tracks (mirroring `above_skip`/`left_skip`'s own leaf approximation
    // just above).
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    // lane-cdffwd2: this frame header's own `reduced_tx_set` bit -- see
    // [`decode_inter_frame_tile_with_cdfs`]'s own doc.
    reduced_tx_set: bool,
    // lane-motionmode round 3: this frame header's own `is_motion_mode_switchable`/
    // `allow_warped_motion` bits (spec 5.11.24's `read_motion_mode`), plus
    // the fixed/switchable interp filter kind `obmc_blend`'s own neighbour
    // reads need (see [`decode_inter_block`]'s own doc) -- this leaf's own
    // prediction is always `Regular` (documented corner-cut, `mc::predict`
    // has no filter param), but an OBMC neighbour can be a real 16x16+
    // block that DID resolve a switchable filter.
    interp_fixed: Option<mc::InterpFilterKind>,
    switchable_motion_mode: bool,
    allow_warped_motion: bool,
    // lane-av1comp: `Some((ref1, comp_group_idx, compound_idx))` only for a
    // real `COMPOUND_REFERENCE` leaf, for the caller's own end-of-16x16
    // `record_compound_ctx` call -- mirrors [`decode_inter_block`]'s own
    // `compound_ctx` local, just handed back instead of applied here (this
    // fn's `neighbours` recording happens once for the whole straddling
    // 16x16 block, after every leaf).
    // lane-screen r2: see [`decode_inter_block`]'s own copy of this param --
    // `BLOCK_8X8` (this leaf's fixed size) is always `av1_allow_palette`-
    // eligible (`bsize >= BLOCK_8X8`).
    allow_screen_content_tools: bool,
    // lane-inter8 r1: the leaf's mv stacks used to be built without the
    // frame's sign-bias table (`NO_SIGN_BIAS`), while [`decode_inter_block`]
    // one level up passed it -- it changes the candidate list and with it
    // `new_mv_ctx`/`ref_mv_ctx`, `drl_ctx` and the number of symbols read.
    // Measured: an 8x8 compound NEAR_NEWMV leaf desynced against aomdec's
    // EC_MODE ladder at exactly this block (mi 4,14 of the mandelbrot
    // 8x8-split gate).
    sign_bias_table: &SignBiasTable,
    // lane-inter8 r2: the frame's temporal mv field (spec 7.10.2.8), which
    // [`decode_inter_block`] one level up has always passed and this leaf
    // passed `None` for -- a `use_ref_frame_mvs` frame's stack then holds
    // fewer candidates here than in the reference decoder, which changes the
    // mode contexts AND the number of `drl_mode` bits read (class parsed
    // then discarded).
    tpl_frame: Option<&TplFrameArgs>,
    // lane-scaledref r1: this frame's own coded luma width (spec 7.11.3.3),
    // for the scaled-reference MC this leaf's own single-ref/compound/OBMC
    // predictions take when a stored reference was coded at another width
    // (`use_superres`) -- mirrors [`decode_inter_block`]'s own param.
    frame_size: (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> Result<(
    bool,
    bool,
    bool,
    Option<(i8, i8, u8, u8)>,
    [u8; 2],
    (i8, Option<i8>),
)> {
    // Round 27: this is the function whose mode read carries the block's real
    // mi (the `EC_MODE_VAL8` rung prints it). The `decode_inter_block`
    // entry sets the same label from ITS `at`, which is a different block --
    // publishing from here is what makes the EC_SYMR mi trustworthy on the
    // inter path, which is what round 22 got wrong by assuming it was not.
    crate::msac::SymbolDecoder::set_symr_phase("inter8");
    crate::msac::SymbolDecoder::set_symr_mi(leaf_mi.0 as i64, leaf_mi.1 as i64);
    const LAST_FRAME: i8 = 1;
    // lane-gmaffine r2: the leaf's OWN switchable-filter symbols, handed back
    // so the caller stamps them into `Neighbours` instead of the `[3, 3]`
    // placeholder. `[3, 3]` is the "intra neighbour, no filter" sentinel: an
    // OBMC blend that picks such a neighbour up feeds it to
    // `neighbour_filter`, which PANICS on it (`from_switchable_symbol`) --
    // that was the warp gate's crash, not an entropy desync. Stays `[3, 3]`
    // on the intra and compound paths.
    let mut leaf_filter_syms = [3u8; 2];
    // lane-gmaffine r1: this leaf's GLOBALMV vector AND the mv stack's
    // missing-candidate fallback both come from the frame's global motion
    // evaluated at THIS block's centre (gm r6's root cause).
    let gm_table = build_gm_mv_table(
        global_motion,
        leaf_mi.0,
        leaf_mi.1,
        2,
        2,
        allow_high_precision_mv,
        force_integer_mv,
    );
    /// `Y_MODE`'s size group (`common_data.h`'s `size_group_lookup[BLOCK_8X8]`).
    const SIZE_GROUP_8: usize = 1;

    let (px, py) = (leaf_mi.1 * MI, leaf_mi.0 * MI);
    let (cpx, cpy) = (px >> ss_x(fctx), py >> ss_y(fctx));
    // lane-txselect: whether this leaf's `TxMode::Select` var-tx tree took
    // the one split a `BLOCK_8X8` can take (`read_block_tx_size` below) --
    // read back by the luma residual read and the tail's neighbour/loop-filter
    // bookkeeping. Always `false` outside a `TxMode::Select` inter frame.
    let split8;
    const SIDE: usize = 8;
    // lane-av1-444 / lane-av1-422sub8rect: this 8x8 leaf's chroma plane block
    // is `ss_size_lookup[BLOCK_8X8][ss_x][ss_y]` -- 4:2:0 BLOCK_4X4 (one
    // TX_4X4 unit), 4:4:4 BLOCK_8X8 (ONE TX_8X8 unit, `av1_get_max_uv_txsize`
    // at ss 0/0, measured against aomdec's EC_COEFF on `av1-profile1-444.ivf`
    // frame 1), and 4:2:2 BLOCK_4X8 (ONE TX_4X8 RECT unit per plane, measured
    // on the pinned t422 fixture: frame 1 mi(0,0) reads plane 1 tx_size 5 ==
    // TX_4X8 at rng 55168 right after the agreed luma TX_8X8 txb_skip -- the
    // old `SIDE >> ss_x` square read was the first-divergence defect this
    // port fixes).
    let chroma_w: usize = SIDE >> ss_x(fctx);
    let chroma_h: usize = SIDE >> ss_y(fctx);
    let chroma_422 = ss_x(fctx) == 1 && ss_y(fctx) == 0;
    let (chroma_set, chroma_scan): (TxbSet, &[u16]) = if chroma_422 {
        // `get_txsize_entropy_ctx(TX_4X8)`: the rect fills its plane block,
        // so the tables are the rect set and the SCAN_4X8 walk, exactly the
        // pairing the key-frame BLOCK_4X8 arm (`decode_block`'s lane-av1-422f
        // table) reads byte-exact.
        (TxbSet::ChromaRect8x4, &SCAN_4X8[..])
    } else if chroma_w == 8 {
        (TxbSet::Chroma8, scan8)
    } else {
        (TxbSet::Chroma4, scan4)
    };
    let chroma_tx = if chroma_w == 8 { TX8 } else { TX4 };

    let (r, c) = outer_at;
    // lane-inter8 r2: this leaf's OWN mi cell, not the enclosing 16x16's --
    // leaf (mi_r, mi_c + 2)'s left neighbour is leaf (mi_r, mi_c), and the
    // left column of a 16x16's second leaf ROW is the previous 16x16 block's
    // bottom leaf, neither of which a SUB-grid slot can name.
    let (rmi, cmi) = leaf_mi;
    // lane-inter8 r2: no sibling-leaf override any more -- each leaf stamps
    // its own 2x2-mi span into the bands right after it decodes (the caller's
    // `record_inter_rect_mi`), so a plain mi read already names the correct
    // neighbour, including one belonging to the previous 16x16 block.
    let above_skip = neighbours.above_skip[cmi];
    let left_skip = neighbours.left_skip[rmi];
    let above_inter = neighbours.above_inter[cmi];
    let left_inter = neighbours.left_inter[rmi];
    let above_ref0 = neighbours.above_ref[cmi];
    let above_ref1 = neighbours.above_ref1[cmi];
    let left_ref0 = neighbours.left_ref[rmi];
    let left_ref1 = neighbours.left_ref1[rmi];
    let skip_mode_ctx =
        usize::from(neighbours.above_skip_mode[cmi]) + usize::from(neighbours.left_skip_mode[rmi]);
    // Same two `inter_segment_id` positions as [`decode_inter_block`]; this
    // leaf is always `BLOCK_8X8`, i.e. 2x2 mi.
    inter_segment_id(dec, cdfs, leaf_mi.0, leaf_mi.1, 2, 2, false, true, fctx);
    let skip_mode = skip_mode_present && dec.symbol(&mut cdfs.skip_mode[skip_mode_ctx]) == 1;
    if skip_mode {
        hit!(SKIP_MODE_HITS);
    }

    let skip_ctx = usize::from(above_skip) + usize::from(left_skip);
    let skip = skip_mode || dec.symbol(&mut cdfs.skip[skip_ctx]) == 1;
    inter_segment_id(dec, cdfs, leaf_mi.0, leaf_mi.1, 2, 2, skip, false, fctx);
    maybe_read_cdef_idx(dec, leaf_mi.0, leaf_mi.1, (2, 2), skip, fctx);
    // Always a `BLOCK_8X8` leaf here, never the whole superblock.
    maybe_read_delta_q(dec, cdfs, leaf_mi.0, leaf_mi.1, false, skip, fctx);
    maybe_read_delta_lf(dec, cdfs, leaf_mi.0, leaf_mi.1, false, skip, fctx);

    let (has_above, has_left) = (
        leaf_mi.0 > neighbours.tile_row0_mi,
        leaf_mi.1 > neighbours.tile_col0_mi,
    );
    let ii_ctx = intra_inter_ctx(has_above, has_left, above_inter, left_inter);
    let is_inter = skip_mode || dec.symbol(&mut cdfs.intra_inter[ii_ctx]) == 1;
    // lane-inter8 r1: mirrors the oracle's `EC_MODE` (instrument rung 4,
    // `read_inter_block_mode_info`) so an 8x8 leaf's mode-info range ladder
    // diffs line for line against a real aomdec.
    if crate::envflags::env_flag!("EC_TRACE_MODE") {
        eprintln!(
            "EC_MODE mi_row={} mi_col={} rng={}",
            leaf_mi.0,
            leaf_mi.1,
            dec.debug_state().0
        );
    }

    let mode_for_tx;
    // lane-uv8 r1: this leaf's chroma prediction mode (`get_uv_mode` of the
    // coded `uv_mode`), for the neighbour bands at the tail. An INTER leaf
    // stores `UV_DC_PRED`, exactly as [`decode_inter_block`] one level up.
    let mut uv_predict_mode = DC_PRED;
    let (luma_grid, u_grid, v_grid);
    // lane-av1-llinter: set when a lossless 4:4:4 leaf's chroma went through
    // the per-unit walk (`read_inter_chroma_lossless`) -- the walk stamps each
    // TX_4X4 unit's own coefficient context, so the tail's whole-block
    // `record_mi` gets its chroma half saved/restored around, exactly like
    // `saved_luma_ctx` does for a `split8` luma.
    let mut chroma_stamped_per_unit = false;
    let mut compound_ctx8: Option<(i8, i8, u8, u8)> = None;
    // lane-inter8 r1: this leaf's own resolved references, handed back so the
    // NEXT leaf of the same 16x16 can use them as its real above/left
    // neighbour (`Neighbours`' arrays only move once per 16x16 block).
    // `(0, None)` is intra.
    let mut leaf_refs: (i8, Option<i8>) = (0, None);
    // lane-t900 r23: this leaf's own palette state for the neighbour bands
    // (`(y_size, y_colors, uv_size, uv_colors)`), all-zero for an inter leaf
    // and for an intra one that codes no palette -- same publication the
    // >=16 intra-in-inter block makes through [`record_strip_palette`].
    let mut palette_record8: (usize, [u16; 8], usize, [u16; 8]) = (0, [0u16; 8], 0, [0u16; 8]);
    if is_inter {
        if reference_select || skip_mode {
            // lane-inter8 r1: the REAL neighbour references, not a
            // `LAST_FRAME`-or-intra stand-in. `av1_get_reference_mode_context`
            // and `read_compound_ref_frames`' own contexts both ask whether a
            // neighbour is COMPOUND, so pretending every inter neighbour is
            // single-ref LAST picked the wrong `comp_mode` CDF row -- measured
            // as an 8x8 leaf reading single-ref where aomdec read compound
            // NEAR_NEWMV (mi 4,14).
            let above_nbr = has_above.then(|| NeighbourRef {
                is_inter: above_inter,
                ref0: if above_inter { above_ref0 } else { 0 },
                ref1: above_ref1,
                uni: above_ref1.is_some_and(|r1| is_uni_comp_ref(above_ref0, r1)),
            });
            let left_nbr = has_left.then(|| NeighbourRef {
                is_inter: left_inter,
                ref0: if left_inter { left_ref0 } else { 0 },
                ref1: left_ref1,
                uni: left_ref1.is_some_and(|r1| is_uni_comp_ref(left_ref0, r1)),
            });
            if skip_mode || read_comp_mode(dec, cdfs, above_nbr, left_nbr) {
                let (ref0, ref1) = if skip_mode {
                    (skip_mode_frame[0] as i8, skip_mode_frame[1] as i8)
                } else {
                    // lane-interp3 r2 (class twin-functions-drift): this leaf
                    // passed a hard-coded `LAST_FRAME` for every inter
                    // neighbour -- the old "coarse LAST_FRAME-or-intra"
                    // approximation -- while the side bands have carried the
                    // neighbour's REAL reference per mi since lane-inter8 r2.
                    // Every `av1_collect_neighbors_ref_counts` vote below
                    // (`uni_comp_ref_p1`, `comp_ref`, `comp_bwdref`, ...) was
                    // therefore taken off the wrong CDF row. Same two args
                    // [`decode_inter_block`]'s own compound arm passes
                    // (`-1` when the band is unavailable is start_tile's /
                    // start_row's own reset value, so no `has_above` guard).
                    read_compound_ref_frames(dec, cdfs, above_nbr, left_nbr, above_ref0, left_ref0)
                };
                leaf_refs = (ref0, Some(ref1));
                let (mi_row, mi_col) = leaf_mi;
                // spec 7.10.2.8, mirroring [`decode_inter_block`]'s own
                // `comp_tpl` build (`use_ref_frame_mvs` frames only).
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
                // lane-av1comp: this leaf path has neither a real sign-bias
                // table nor `tpl_frame` (see this function's own doc on the
                // coarse `LAST_FRAME`-or-intra neighbour approximation
                // already in force above), matching the plain `find_mv_stack`
                // (no bias, no tpl) the leaf's single-ref arm below already
                // uses.
                let comp_stack = crate::mvstack::find_mv_stack_compound(
                    grid,
                    mi_row,
                    mi_col,
                    2,
                    2,
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
                    read_inter_compound_mode(
                        dec,
                        cdfs,
                        comp_stack.new_mv_ctx,
                        comp_stack.ref_mv_ctx,
                    )
                };
                if crate::envflags::env_flag!("EC_TRACE_MODE") {
                    eprintln!(
                        "EC_LEAFMODE mi_row={} mi_col={} cmode={} stack={} newmv_ctx={} refmv_ctx={} rng={}",
                        leaf_mi.0,
                        leaf_mi.1,
                        compound_mode + 17,
                        comp_stack.entries.len(),
                        comp_stack.new_mv_ctx,
                        comp_stack.ref_mv_ctx,
                        dec.debug_state().0
                    );
                }
                let (mv0, mv1) = assign_compound_mv(
                    dec,
                    cdfs,
                    &comp_stack,
                    compound_mode,
                    allow_high_precision_mv,
                    force_integer_mv,
                    // lane-inter8 r1: the real per-ref gm mvs, same as
                    // [`decode_inter_block`]'s own compound arm.
                    (
                        gm_table[(ref0 - LAST_FRAME) as usize],
                        gm_table[(ref1 - LAST_FRAME) as usize],
                    ),
                    fctx,
                );
                // lane-inter8 r3: byte-format-identical to the oracle's
                // rung-4 `EC_MODE_VAL`, so the leaf's mode+mv VALUE ladder
                // diffs line for line against a real aomdec.
                if crate::envflags::env_flag!("EC_TRACE_MODE") {
                    eprintln!(
                        "EC_MODE_VAL mi_row={} mi_col={} mode={} ref0={} ref1={} mv0=({},{}) rng={}",
                        leaf_mi.0,
                        leaf_mi.1,
                        compound_mode + 17,
                        ref0,
                        ref1,
                        mv0.0,
                        mv0.1,
                        dec.debug_state().0
                    );
                }

                // spec 5.11.25: same gating [`decode_inter_block`]'s own
                // compound arm uses, keyed off `outer_at`'s [`SUB`]-grid
                // slot -- the only granularity `Neighbours`' comp_group_idx/
                // compound_idx arrays track.
                let group_ctx = get_comp_group_idx_context(neighbours, leaf_mi, SIDE);
                let comp_group_idx = if !skip_mode
                    && enable_masked_compound
                    && is_any_masked_compound_used_here(SIDE, SIDE)
                {
                    crate::msac::SymbolDecoder::set_symr_cdf("comp_group_idx");
                    dec.symbol(&mut cdfs.comp_group_idx[group_ctx])
                } else {
                    0
                };
                // lane-maskcomp r2 / lane-wedge r3: see the 16x16/32x32
                // leaf's own comment.
                let mut diffwtd_mask_type: Option<u8> = None;
                let mut wedge_mask: Option<&'static [u8]> = None;
                if comp_group_idx == 1 {
                    // lane-maskcomp r1: see the 16x16 leaf's own comment
                    // above -- SIDE==8 here, `wedge_bsize` index 3
                    // (BLOCK_8X8, `wedge_types > 0`).
                    let compound_type = dec.symbol(&mut cdfs.compound_type[3]);
                    if compound_type == 0 {
                        // COMPOUND_WEDGE: lane-wedge r3, see the 16x16/32x32
                        // leaf's own comment.
                        let wedge_index = dec.symbol(&mut cdfs.wedge_idx[3]);
                        let wedge_sign = dec.literal(1);
                        hit!(WEDGE_HITS);
                        wedge_mask = Some(
                            crate::wedge::wedge_masks()
                                .codebook(SIDE, SIDE)
                                .mask(wedge_sign as usize, wedge_index),
                        );
                    } else {
                        let mask_type = dec.literal(1);
                        diffwtd_mask_type = Some(mask_type as u8);
                    }
                    hit!(MASKED_COMPOUND_HITS);
                }
                // corner-cut (lane-av1comp r16/r17, lane-av1blend r1): same
                // reference-slot defect as the 16x16 leaf's own mask above
                // (r1: falsified as an MC/blend math bug, isolated to a DPB
                // slot/refresh_frame_flags issue around BWDREF_FRAME) -- see
                // that comment.
                // r3: same defect as the 16x16 leaf's own mask above -- see
                // that comment (compound_idx's decoded value, not the blend
                // math or reference-slot bookkeeping). r6/r7: same re-mask
                // as the 16x16 leaf above -- one real bug found and fixed
                // (mvstack.rs ref_ctx family), a second, still-unfound
                // compound_idx defect surfaces on the wide gate sweep.
                // lane-av1idx r1: same re-mask as the 16x16 leaf above --
                // see that comment.
                let (fwd_offset, bck_offset, compound_idx) =
                    if comp_group_idx == 0 && !skip_mode && enable_jnt_comp {
                        let idx_ctx = get_comp_index_context(
                            neighbours,
                            leaf_mi,
                            SIDE,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints[(ref0 - LAST_FRAME) as usize],
                            ref_order_hints[(ref1 - LAST_FRAME) as usize],
                        );
                        crate::msac::SymbolDecoder::set_symr_cdf("compound_idx");
                        let idx = dec.symbol(&mut cdfs.compound_idx[idx_ctx]);
                        if idx == 1 {
                            (8, 8, 1u8)
                        } else {
                            // lane-av1distwtd: the 8x8 leaf's own
                            // distance-weighted arm, same counter as the
                            // 16x16+ site above.
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
                compound_ctx8 = Some((ref0, ref1, comp_group_idx as u8, compound_idx));
                if crate::envflags::env_flag!("EC_AV1_COMPIDX_DUMP") {
                    eprintln!(
                        "EC_COMPIDX mi_row={mi_row} mi_col={mi_col} bsize=8 mode={compound_mode} mv0=({},{}) mv1=({},{}) ref0={ref0} ref1={ref1} comp_group_idx={comp_group_idx} compound_idx={compound_idx} tell={}",
                        mv0.0,
                        mv0.1,
                        mv1.0,
                        mv1.1,
                        dec.debug_bitpos()
                    );
                }

                // lane-interp3 r1: `read_mb_interp_filter` (libaom
                // `decodemv.c` 1575, the tail of
                // `read_inter_block_mode_info`) runs for EVERY inter block,
                // compound included, right AFTER `read_compound_type` -- this
                // leaf read no filter symbol at all, so a SWITCHABLE-filter
                // stream lost one symbol (two under `enable_dual_filter`) at
                // every compound 8x8 leaf, and the leaf recorded the `[3, 3]`
                // "no filter" sentinel, which made a neighbouring OBMC block's
                // `neighbour_filter` PANIC instead of blending.
                // `av1_is_interp_needed` suppression terms for this arm:
                // `skip_mode`, and `is_nontrans_global_motion` (mode
                // GLOBAL_GLOBALMV with BOTH refs' models non-TRANSLATION) --
                // WARPED_CAUSAL cannot occur on a compound block.
                let gm_nontrans_c = compound_mode == 6
                    && global_motion[(ref0 - LAST_FRAME) as usize].model
                        != ec_av1_syntax::WarpModel::Translation
                    && global_motion[(ref1 - LAST_FRAME) as usize].model
                        != ec_av1_syntax::WarpModel::Translation;
                // spec `get_ref_filter_type`, keyed on this block's ref0 and
                // matching EITHER of the neighbour's references -- same shape
                // as [`decode_inter_block`]'s compound arm. Same corner-cut as
                // this leaf's single-ref arm below: the context comes from the
                // enclosing 16x16's coarse slot.
                let above_filter_ctx = if neighbours.above_ref[cmi] == ref0
                    || neighbours.above_ref1[cmi] == Some(ref0)
                {
                    neighbours.above_filter[cmi]
                } else {
                    [3, 3]
                };
                let left_filter_ctx = if neighbours.left_ref[rmi] == ref0
                    || neighbours.left_ref1[rmi] == Some(ref0)
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
                    gm_nontrans_c || skip_mode,
                    above_filter_ctx,
                    left_filter_ctx,
                    true,
                );
                leaf_filter_syms = resolved_filter;
                if interp_fixed.is_none() && !(gm_nontrans_c || skip_mode) {
                    hit!(COMPOUND8_FILTER_HITS);
                }

                // lane-defer8: both slots' SIZES at parse, pictures in the closure.
                let scale0 = scale_of(ref_dims(ref0, refpix)?, frame_size);
                let scale1 = scale_of(ref_dims(ref1, refpix)?, frame_size);
                if !scale0.is_unscaled() || !scale1.is_unscaled() {
                    hit!(SCALED_COMPOUND_HITS);
                    hit!(SCALED_BLOCK8_HITS);
                }

                // lane-interp3 r2 (class twin-functions-drift): both slots
                // used to be stamped `false`, while [`decode_inter_block`]'s
                // compound arm computes libaom's PER-SLOT `is_global_mv_block`
                // (blockd.h:421-429 -- mode GLOBAL_GLOBALMV, block >= 8x8
                // (always true for this leaf) and THAT slot's own gm model >
                // TRANSLATION). A neighbour carrying the flag makes
                // `setup_ref_mv_list` substitute the READER's global mv for
                // the neighbour's stored one, so a cleared flag fed the next
                // block's stack a stored mv where libaom has the global
                // candidate -- a wrong mv stack, wrong mode contexts, and a
                // desync one block later.
                let is_globalmv_c = compound_mode == 6;
                let is_global_mv0_c =
                    is_globalmv_c && global_motion[(ref0 - LAST_FRAME) as usize].model as u8 > 1;
                let is_global_mv1_c =
                    is_globalmv_c && global_motion[(ref1 - LAST_FRAME) as usize].model as u8 > 1;
                // lane-interp3 r3 (class twin-functions-drift, second
                // instance): libaom applies `allow_warp`'s GLOBAL branch
                // (`reconinter.c:33-55`, reached from `av1_init_warp_params`
                // inside `build_inter_predictors_8x8_and_bigger`'s per-`ref`
                // loop) to EVERY `is_global_mv_block` slot, compound
                // included and independently of `motion_mode` -- the 16x16+
                // compound arm has done this since lane-cwarp r1, this leaf
                // predicted both taps translationally, so a GLOBAL_GLOBALMV
                // 8x8 compound leaf under a non-TRANSLATION model drifted by
                // a few luma levels, growing away from the block origin.
                // `force_integer_mv` short-circuits `av1_init_warp_params`
                // before `allow_warp`; chroma is 4x4 here, below
                // `av1_init_warp_params`'s own 8px per-plane bound, so only
                // luma warps (same rule as this leaf's single-ref arm).
                let leaf_compound_warp = |ref_frame: i8, eligible: bool| {
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
                // lane-t900 r28: same per-slot `allow_warp` scale bail-out as
                // the 16x16-and-bigger compound path above.
                let leaf_ref_unscaled = |rf: i8| {
                    refpix
                        .dims(rf)
                        .is_some_and(|d| scale_of(d, frame_size).is_unscaled())
                };
                let warp0_c =
                    leaf_compound_warp(ref0, is_global_mv0_c).filter(|_| leaf_ref_unscaled(ref0));
                let warp1_c =
                    leaf_compound_warp(ref1, is_global_mv1_c).filter(|_| leaf_ref_unscaled(ref1));
                if warp0_c.is_some() || warp1_c.is_some() {
                    hit!(COMPOUND_WARP_HITS);
                    hit!(COMPOUND_WARP_HITS_8);
                }
                grid.fill_rect(
                    mi_row,
                    mi_col,
                    2,
                    2,
                    MiInfo {
                        is_inter: true,
                        ref_frame: ref0,
                        ref_frame1: ref1,
                        mv1: mv16(mv1),
                        mv: mv16(mv0),
                        is_new_mv: matches!(compound_mode, 2 | 3 | 4 | 5 | 7),
                        size: 2,
                        size_h: 2,
                        is_global_mv0: is_global_mv0_c,
                        is_global_mv1: is_global_mv1_c,
                    },
                );
                mode_for_tx = 0;

                // lane-defer8: this leaf's whole prediction as a closure, run at the
                // push site like [`decode_inter_block`]'s.
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
                    let mut inter0_y = vec![0i32; SIDE * SIDE];
                    mc::predict_compound_intermediate(
                        &py0.data,
                        py0.width,
                        py0.true_width,
                        py0.true_height,
                        mv_to_q4(px, mv0.1, 0),
                        mv_to_q4(py, mv0.0, 0),
                        scale0,
                        SIDE,
                        SIDE,
                        h_filter,
                        v_filter,
                        &mut inter0_y,
                        fctx,
                    );
                    if let Some(wp) = &warp0_c {
                        crate::warp::warp_affine_compound(
                            wp,
                            &py0.data,
                            py0.true_width as i32,
                            py0.true_height as i32,
                            py0.width as i32,
                            &mut inter0_y,
                            px as i32,
                            py as i32,
                            SIDE as i32,
                            SIDE as i32,
                            SIDE as i32,
                            0,
                            0,
                            fctx,
                        );
                    }
                    let mut inter1_y = vec![0i32; SIDE * SIDE];
                    mc::predict_compound_intermediate(
                        &py1.data,
                        py1.width,
                        py1.true_width,
                        py1.true_height,
                        mv_to_q4(px, mv1.1, 0),
                        mv_to_q4(py, mv1.0, 0),
                        scale1,
                        SIDE,
                        SIDE,
                        h_filter,
                        v_filter,
                        &mut inter1_y,
                        fctx,
                    );
                    if let Some(wp) = &warp1_c {
                        crate::warp::warp_affine_compound(
                            wp,
                            &py1.data,
                            py1.true_width as i32,
                            py1.true_height as i32,
                            py1.width as i32,
                            &mut inter1_y,
                            px as i32,
                            py as i32,
                            SIDE as i32,
                            SIDE as i32,
                            SIDE as i32,
                            0,
                            0,
                            fctx,
                        );
                    }
                    let mut pred_y = refill(std::mem::take(&mut out[0]), SIDE * SIDE);
                    let mut diffwtd_mask_y;
                    let mask_y: Option<&[u8]> = if let Some(mask_type) = diffwtd_mask_type {
                        diffwtd_mask_y = vec![0u8; SIDE * SIDE];
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
                            SIDE,
                            SIDE,
                            SIDE,
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

                    let mut inter0_u = vec![0i32; chroma_w * chroma_h];
                    mc::predict_compound_intermediate(
                        &pu0.data,
                        pu0.width,
                        pu0.true_width,
                        pu0.true_height,
                        mv_to_q4(cpx, mv0.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv0.0, ss_y(fctx)),
                        scale0,
                        chroma_w,
                        chroma_h,
                        h_filter,
                        v_filter,
                        &mut inter0_u,
                        fctx,
                    );
                    // lane-av1444altref r3: this leaf warped its LUMA
                    // intermediates (`warp0_c`/`warp1_c` above) but left both
                    // CHROMA arms translational.  libaom warps per PLANE:
                    // `build_inter_predictors_8x8_and_bigger`
                    // (reconinter_template.inc:230) runs the whole
                    // `av1_init_warp_params` + `av1_warp_plane` pair for every
                    // plane of the block, chroma included, with
                    // `pd->subsampling_x/y` and the plane's own origin/extent.
                    // A global-motion (warp) 8x8 leaf at 4:4:4 therefore had
                    // its chroma predicted from the block's top-left
                    // translation instead of the affine map -- the MV is the
                    // affine's local derivative, so the two agree at the
                    // block's top-left corner and drift apart toward its
                    // bottom-right, which is exactly the measured signature
                    // (a bottom-right-biased +/-1 gradient in 8x8 chroma
                    // cells of the 4:4:4 altref cell `ll444_c`, luma exact).
                    // `warp_plane_allowed` mirrors `av1_init_warp_params`'s
                    // `block_width < 8 || block_height < 8` bail, so a
                    // subsampled plane's sub-8 chroma block stays
                    // translational exactly as the 16x16+ arm above does.
                    if let Some(wp) = &warp0_c
                        && warp_plane_allowed(chroma_w, chroma_h)
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
                            chroma_w as i32,
                            chroma_h as i32,
                            chroma_w as i32,
                            ss_x(fctx) as i32,
                            ss_y(fctx) as i32,
                            fctx,
                        );
                    }
                    let mut inter1_u = vec![0i32; chroma_w * chroma_h];
                    mc::predict_compound_intermediate(
                        &pu1.data,
                        pu1.width,
                        pu1.true_width,
                        pu1.true_height,
                        mv_to_q4(cpx, mv1.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv1.0, ss_y(fctx)),
                        scale1,
                        chroma_w,
                        chroma_h,
                        h_filter,
                        v_filter,
                        &mut inter1_u,
                        fctx,
                    );
                    if let Some(wp) = &warp1_c
                        && warp_plane_allowed(chroma_w, chroma_h)
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
                            chroma_w as i32,
                            chroma_h as i32,
                            chroma_w as i32,
                            ss_x(fctx) as i32,
                            ss_y(fctx) as i32,
                            fctx,
                        );
                    }
                    let mut pred_u = refill(std::mem::take(&mut out[1]), chroma_w * chroma_h);
                    if let Some(mask_y) = mask_y {
                        mc::blend_masked_compound(
                            &inter0_u,
                            &inter1_u,
                            mask_y,
                            SIDE,
                            chroma_w,
                            chroma_h,
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

                    let mut inter0_v = vec![0i32; chroma_w * chroma_h];
                    mc::predict_compound_intermediate(
                        &pv0.data,
                        pv0.width,
                        pv0.true_width,
                        pv0.true_height,
                        mv_to_q4(cpx, mv0.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv0.0, ss_y(fctx)),
                        scale0,
                        chroma_w,
                        chroma_h,
                        h_filter,
                        v_filter,
                        &mut inter0_v,
                        fctx,
                    );
                    // lane-av1444altref r3: the V arm's missing per-plane warp,
                    // same defect and same fix as U above.
                    if let Some(wp) = &warp0_c
                        && warp_plane_allowed(chroma_w, chroma_h)
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
                            chroma_w as i32,
                            chroma_h as i32,
                            chroma_w as i32,
                            ss_x(fctx) as i32,
                            ss_y(fctx) as i32,
                            fctx,
                        );
                    }
                    let mut inter1_v = vec![0i32; chroma_w * chroma_h];
                    mc::predict_compound_intermediate(
                        &pv1.data,
                        pv1.width,
                        pv1.true_width,
                        pv1.true_height,
                        mv_to_q4(cpx, mv1.1, ss_x(fctx)),
                        mv_to_q4(cpy, mv1.0, ss_y(fctx)),
                        scale1,
                        chroma_w,
                        chroma_h,
                        h_filter,
                        v_filter,
                        &mut inter1_v,
                        fctx,
                    );
                    if let Some(wp) = &warp1_c
                        && warp_plane_allowed(chroma_w, chroma_h)
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
                            chroma_w as i32,
                            chroma_h as i32,
                            chroma_w as i32,
                            ss_x(fctx) as i32,
                            ss_y(fctx) as i32,
                            fctx,
                        );
                    }
                    let mut pred_v = refill(std::mem::take(&mut out[2]), chroma_w * chroma_h);
                    if let Some(mask_y) = mask_y {
                        mc::blend_masked_compound(
                            &inter0_v,
                            &inter1_v,
                            mask_y,
                            SIDE,
                            chroma_w,
                            chroma_h,
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

                    *out = [pred_y, pred_u, pred_v];
                };
                let (pred_y, pred_u, pred_v): (Vec<u16>, Vec<u16>, Vec<u16>);
                let (sy, su, sv) = if pred_deferred(refpix) {
                    push_pred(build);
                    (Pred::Cur { plane: 0, stride: SIDE, off: 0 }, Pred::Cur { plane: 1, stride: chroma_w, off: 0 }, Pred::Cur { plane: 2, stride: chroma_w, off: 0 })
                } else {
                    let mut built: [Vec<u16>; 3] = Default::default();
                    build(y, u, v, refpix, fctx, &mut built);
                    let [by, bu, bv] = built;
                    (pred_y, pred_u, pred_v) = (by, bu, bv);
                    (
                        Pred::Inline(&pred_y, SIDE),
                        Pred::Inline(&pred_u, chroma_w),
                        Pred::Inline(&pred_v, chroma_w),
                    )
                };
                split8 = read_block_tx_size(
                    dec,
                    cdfs,
                    neighbours,
                    leaf_mi,
                    SIDE,
                    (mi_cols as usize, mi_rows as usize),
                    true,
                    skip,
                    fctx,
                )?
                .1
                .is_some();
                if skip {
                    push_mc(
                        y,
                        u,
                        v,
                        0,
                        px,
                        py,
                        SIDE,
                        sy,
                        &ZERO_RESIDUAL[..SIDE * SIDE],
                        fctx,
                    );
                    // lane-av1-422sub8rect: the plane block is the (chroma_w,
                    // chroma_h) rect at 4:2:2 -- the recorded write is
                    // stride `chroma_w`, full `chroma_h` rows.
                    push_mc_rect(
                        y,
                        u,
                        v,
                        1,
                        cpx,
                        cpy,
                        chroma_w,
                        chroma_w,
                        chroma_h,
                        su,
                        &ZERO_RESIDUAL[..chroma_w * chroma_h],
                        fctx,
                    );
                    push_mc_rect(
                        y,
                        u,
                        v,
                        2,
                        cpx,
                        cpy,
                        chroma_w,
                        chroma_w,
                        chroma_h,
                        sv,
                        &ZERO_RESIDUAL[..chroma_w * chroma_h],
                        fctx,
                    );
                    luma_grid = Grid::Zero(SIDE * SIDE);
                    u_grid = Grid::Zero(chroma_w * chroma_h);
                    v_grid = Grid::Zero(chroma_w * chroma_h);
                } else {
                    let around = neighbours.around_mi(leaf_mi, 8);
                    // lane-av1-422sub8rect: the 4:2:2 chroma unit is the
                    // TX_4X8 rect -- its `txb_skip_ctx` reads per chroma
                    // 4-px cell ([`Neighbours::around_mi_422_chroma`], the
                    // key-frame BLOCK_4X8 arm's own context walk), and the
                    // coefficients go through the inter rect reader with the
                    // inherited luma tx type.
                    let chroma_around = if chroma_422 {
                        neighbours.around_mi_422_chroma(leaf_mi, 8, 8)
                    } else {
                        around
                    };
                    let (grid8, luma_tx_type) = read_inter_luma8(
                        y,
                        u,
                        v,
                        dec,
                        cdfs,
                        neighbours,
                        leaf_mi,
                        px,
                        py,
                        sy,
                        mode_for_tx,
                        base_q_idx,
                        scan8,
                        scan4,
                        reduced_tx_set,
                        split8,
                        fctx,
                    )?;
                    luma_grid = grid8;
                    // Lossless 8x8 inter leaf: `av1_get_tx_size` returns
                    // TX_4X4 for EVERY plane of a lossless block
                    // (`decodeframe.c:1197`), so the chroma plane block is a
                    // raster of TX_4X4 units whenever it is BIGGER than one --
                    // 4:4:4's 8x8 (2x2) and 4:2:2's 4x8 (1x2). Only 4:2:0's
                    // 4x4 keeps the single-unit read, where
                    // `plane_bsize == txsize_to_bsize[tx_size]` puts
                    // `get_txb_ctx`'s ctx_offset at 7.
                    if lossless(fctx) && (chroma_w > 4 || chroma_h > 4) && !mono(fctx) {
                        let (ug, vg) = leaf8_inter_chroma_lossless(
                            y,
                            u,
                            v,
                            dec,
                            cdfs,
                            neighbours,
                            su,
                            sv,
                            leaf_mi,
                            (cpx, cpy),
                            (chroma_w, chroma_h),
                            chroma_w,
                            base_q_idx,
                            fctx,
                        )?;
                        u_grid = ug;
                        v_grid = vg;
                        chroma_stamped_per_unit = true;
                    } else {
                        u_grid = if chroma_422 {
                            read_inter_plane_rect(
                                dec,
                                cdfs,
                                chroma_set,
                                (chroma_w, chroma_h),
                                chroma_w,
                                1,
                                chroma_around[1],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                cpx,
                                cpy,
                                su,
                                Some(luma_tx_type),
                                None,
                                fctx,
                            )?
                            .0
                        } else {
                            read_inter_plane(
                                dec,
                                cdfs,
                                chroma_set,
                                chroma_scan,
                                1,
                                chroma_around[1],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                cpx,
                                cpy,
                                chroma_w,
                                base_q_idx,
                                su,
                                Some(luma_tx_type),
                                None,
                                fctx,
                            )?
                            .0
                        };
                        v_grid = if chroma_422 {
                            read_inter_plane_rect(
                                dec,
                                cdfs,
                                chroma_set,
                                (chroma_w, chroma_h),
                                chroma_w,
                                2,
                                chroma_around[2],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                cpx,
                                cpy,
                                sv,
                                Some(luma_tx_type),
                                None,
                                fctx,
                            )?
                            .0
                        } else {
                            read_inter_plane(
                                dec,
                                cdfs,
                                chroma_set,
                                chroma_scan,
                                2,
                                chroma_around[2],
                                mode_for_tx,
                                y,
                                u,
                                v,
                                cpx,
                                cpy,
                                chroma_w,
                                base_q_idx,
                                sv,
                                Some(luma_tx_type),
                                None,
                                fctx,
                            )?
                            .0
                        };
                    }
                }
                // lane-inter8 r2: the COMPOUND leaf used to return here
                // WITHOUT the end-of-function `record_mi`/`fill_lf_grid` the
                // single-ref path falls through to -- so a compound 8x8 leaf
                // left `above_side_mi`/`left_side_mi` (the next block's
                // partition context), the per-plane coefficient level state
                // and the loop-filter grid describing whatever block was
                // there before. Every leaf of the gate's stream is compound,
                // so the 16x16 below a split block read partition ctx 0
                // instead of 1 and mis-read PARTITION_SPLIT as NONE.
                // lane-sub8x4 r1: the same plane-0 save/restore the fall-through
                // (single-ref) path runs below -- a `split8` leaf's plane-0
                // neighbour context is per 4x4 transform unit (written by
                // `record_mi_luma` inside `read_inter_luma8`), and `record_mi`
                // rewrites all three planes from the whole-block grid, which is
                // all-zero in the split case. Missing here, a compound var-tx 8x8
                // leaf published level 0 to its right/below neighbours: the next
                // block's first TU read `skip_contexts[4][0] = 3` where aomdec
                // reads `[4][4] = 6` (class early-return-skips-tail).
                // lane-lossless3: `!skip` too. A SKIP block codes no coefficients and so
                // publishes nothing per transform unit, and on a LOSSLESS frame
                // `read_block_tx_size` hands back 4x4 leaves for a skip block as well
                // (libaom's `read_tx_size` returns TX_4X4 before it reads anything), so
                // `split8` is true over an UNTOUCHED band -- the restore then put the
                // previous block's levels back over the zeros `record_mi` had just
                // written, where libaom runs `av1_reset_entropy_context`. Class
                // [[override-slot-on-one-arm]]: a save/restore is only valid on the arm
                // that actually wrote the per-TU state.
                let saved_luma_ctx = (split8 && !skip).then(|| {
                    (
                        [
                            neighbours.left[leaf_mi.0][0],
                            neighbours.left[leaf_mi.0 + 1][0],
                        ],
                        [
                            neighbours.above[leaf_mi.1][0],
                            neighbours.above[leaf_mi.1 + 1][0],
                        ],
                    )
                });
                // lane-t900 r23: every leaf stamps its palette state (all-zero for an
                // inter leaf or a non-palette intra one, real for a palette block) into
                // the mi-granular neighbour bands, so the next intra block's
                // `av1_get_palette_mode_ctx` and colour cache see exactly what libaom's
                // mode-info grid holds.
                record_strip_palette(
                    neighbours,
                    leaf_mi,
                    8,
                    8,
                    palette_record8.0,
                    palette_record8.1,
                    palette_record8.2,
                    palette_record8.3,
                );
                let saved_chroma_ctx = chroma_stamped_per_unit.then(|| {
                    (
                        [
                            [neighbours.left[leaf_mi.0][1], neighbours.left[leaf_mi.0][2]],
                            [
                                neighbours.left[leaf_mi.0 + 1][1],
                                neighbours.left[leaf_mi.0 + 1][2],
                            ],
                        ],
                        [
                            [
                                neighbours.above[leaf_mi.1][1],
                                neighbours.above[leaf_mi.1][2],
                            ],
                            [
                                neighbours.above[leaf_mi.1 + 1][1],
                                neighbours.above[leaf_mi.1 + 1][2],
                            ],
                        ],
                    )
                });
                neighbours.record_mi(leaf_mi, 8, &[&luma_grid[..], &u_grid[..], &v_grid[..]]);
                if let Some((left, above)) = saved_chroma_ctx {
                    // The walk's per-unit stamps are the block's real chroma
                    // coefficient contexts (`av1_set_contexts` per TX_4X4
                    // unit); put them back over `record_mi`'s composed state.
                    for (cell, uv) in left.into_iter().enumerate() {
                        neighbours.left[leaf_mi.0 + cell][1] = uv[0];
                        neighbours.left[leaf_mi.0 + cell][2] = uv[1];
                    }
                    for (cell, uv) in above.into_iter().enumerate() {
                        neighbours.above[leaf_mi.1 + cell][1] = uv[0];
                        neighbours.above[leaf_mi.1 + cell][2] = uv[1];
                    }
                }
                if let Some((left, above)) = saved_luma_ctx {
                    for (cell, state) in left.into_iter().enumerate() {
                        neighbours.left[leaf_mi.0 + cell][0] = state;
                    }
                    for (cell, state) in above.into_iter().enumerate() {
                        neighbours.above[leaf_mi.1 + cell][0] = state;
                    }
                }
                // lane-cdef r1: ... and the CDEF skip band too. `apply_cdef`'s
                // `is_8x8_block_skip` (libaom `cdef.c:29-38`) reads all four mi
                // cells of the 8x8; this leaf wrote none of them, so a 64x64
                // whose bottom-right 16x16 is an all-skip 8x8-leaf group kept
                // the PREVIOUS frame's flags and got filtered where libaom's
                // dlist excludes it (3 luma samples on a 192x128 inter frame).
                hit!(INTER8_SKIP_BAND_HITS);
                neighbours.fill_skip_grid(leaf_mi, 2, skip);
                // lane-sub8x4 r2: `split8` is this leaf's OWN var-tx split, so
                // the deblock grid must publish the TX_4X4 leaves' width the
                // same way the fall-through arm below does (libaom
                // `get_transform_size`, av1_loopfilter.c:207, reads the per-TU
                // `inter_tx_size` for a non-skip inter luma block). Hardcoding
                // 8 here left a compound var-tx 8x8 leaf's interior vertical
                // edge at x % 8 == 4 unfiltered -- class early-return-skips-tail,
                // third instance in this same arm.
                neighbours.fill_lf_grid(
                    leaf_mi,
                    2,
                    if split8 { 4 } else { 8 },
                    if is_inter {
                        leaf_refs.0.max(LAST_FRAME)
                    } else {
                        0
                    },
                    fctx,
                );
                return Ok((
                    skip,
                    is_inter,
                    skip_mode,
                    compound_ctx8,
                    leaf_filter_syms,
                    leaf_refs,
                ));
            }
        }
        // lane-gmaffine r1: the leaf reads the FULL `single_ref` tree
        // ([`read_single_ref`], the same one the 16x16+ leaf uses, with the
        // real per-neighbour p1..p6 contexts) instead of the old three-symbol
        // LAST-only probe plus a refusal -- aomenc picks GOLDEN/ALTREF for
        // 8x8 leaves constantly, so that refusal was what every 8x8 gate
        // actually stopped at.
        let ref_frame = read_single_ref(
            dec,
            cdfs,
            neighbours.above_ref[cmi],
            neighbours.above_ref1[cmi],
            neighbours.left_ref[rmi],
            neighbours.left_ref1[rmi],
        );
        leaf_refs = (ref_frame, None);
        // lane-defer8: this leaf's reference SIZE at parse, its pictures
        // inside the prediction closure below.
        let (ref_width, ref_height) = ref_dims(ref_frame, refpix)?;

        let (mi_row, mi_col) = leaf_mi;
        // lane-inter8 r2: the frame's temporal mv candidates, keyed on THIS
        // leaf's own reference (lane-gmaffine r1 taught it non-LAST refs).
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
            mi_row,
            mi_col,
            2,
            2,
            ref_frame,
            mi_cols as usize,
            mi_rows as usize,
            sign_bias_table,
            &gm_table,
            tpl,
        );
        // lane-interp3 r2: same per-entry EC_STACK ladder
        // [`decode_inter_block`]'s single-ref arm prints, so this leaf's stack
        // diffs line for line against the oracle's.
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            for (i, e) in stack.entries.iter().enumerate() {
                eprintln!(
                    "EC_STACK mi_row={mi_row} mi_col={mi_col} ref={ref_frame} i={i} this=({},{}) comp=(0,0) w={}",
                    e.mv.0, e.mv.1, e.weight
                );
            }
        }

        let not_new = dec.symbol(&mut cdfs.new_mv[stack.new_mv_ctx]) == 1;
        if !not_new {
            crate::census::mode(0);
        }
        let mut is_globalmv = false;
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
                // lane-gmaffine r1: GLOBALMV (spec 7.10.2.1) -- the same
                // `gm_get_motion_vector` value the 16x16+ leaf reads out of
                // its own `gm_table`, computed at this block's centre.
                hit!(GLOBALMV_HITS_8);
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
        // lane-sb128 r4: `interintra` (spec 5.11.24) at the 8x8 leaf too --
        // see [`decode_inter_block`]'s own doc; `BLOCK_8X8` is always
        // `is_interintra_allowed_bsize`-eligible, single-ref-only branch
        // (compound already returned above), GLOBALMV already refused
        // (round 3), so the only real gate left is the header enable bit.
        let mut interintra_mode: Option<u8> = None;
        let mut wedge_mask: Option<(&'static [u8], usize)> = None;
        if enable_interintra_compound && !skip_mode {
            crate::msac::SymbolDecoder::set_symr_cdf("interintra");
            let interintra = dec.symbol(&mut cdfs.interintra[SIZE_GROUP_8]) == 1;
            if interintra {
                // lane-interintra r1: same read pair as the 16/32 site;
                // wedge cdf row = BLOCK_8X8 (index 3), read on block size
                // alone, never the `enable_interintra_wedge` seq bit.
                let ii = dec.symbol(&mut cdfs.interintra_mode[SIZE_GROUP_8]) as u8;
                let wedge = dec.symbol(&mut cdfs.wedge_interintra[3]) == 1;
                if wedge {
                    // lane-wii r2: same adapting `wedge_index` symbol as the
                    // 16/32 leaf, fixed sign 0 -- see that site's comment.
                    let wedge_index = dec.symbol(&mut cdfs.wedge_idx[3]);
                    hit!(WII_HITS);
                    wedge_mask = Some((
                        crate::wedge::wedge_masks()
                            .codebook(SIDE, SIDE)
                            .mask(0, wedge_index),
                        SIDE,
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
        // lane-motionmode round 3: `read_motion_mode` (spec 5.11.24) at the
        // 8x8 leaf too -- `motion_mode_allowed` still holds here (BLOCK_8X8
        // is the spec's own minimum eligible size, single-ref-only branch,
        // no interintra reader, `IDENTITY`-only global motion). Mirrors the
        // 16x16 leaf's own `decode_inter_block` read (see its doc above) --
        // same 2-symbol `obmc_cdf` alphabet, same `allow_warped_motion=1`
        // refusal (warp needs `av1_findSamples`, not ported at either leaf).
        // lane-gmaffine r1: the two libaom predicates, exactly as the
        // 16x16+ leaf resolves them (see [`decode_inter_block`]'s own doc):
        // `is_global_mv_block` (mode GLOBAL*, model > TRANSLATION, block
        // >= 8x8 -- BLOCK_8X8 IS the 8px minimum, so the size predicate is
        // always true here) suppresses the motion_mode read entirely and
        // turns on the global warp below.
        let gm_ref = &global_motion[(ref_frame - LAST_FRAME) as usize];
        let is_global_mv_block = is_globalmv && gm_ref.model as u8 > 1;
        let motion_mode_eligible = switchable_motion_mode
            && !skip_mode
            && interintra_mode.is_none()
            && !(is_global_mv_block && !force_integer_mv)
            && has_overlappable_neighbour(
                grid,
                mi_row,
                mi_col,
                2,
                2,
                mi_cols as usize,
                mi_rows as usize,
            );
        let mut warp_params: Option<crate::warp::WarpParams> = None;
        let mut warped_selected = false;
        let mut obmc_selected = false;
        if motion_mode_eligible {
            // lane-warp round 1: same 3-vs-2-symbol split as the 16x16+ leaf
            // (see its own doc) -- this leaf is always `LAST_FRAME`-only
            // (grid.set below hardcodes it).
            // lane-scaledref r2 sibling of the 16x16+ leaf's fix: libaom
            // drops WARPED_CAUSAL eligibility (so this reads `obmc_cdf`)
            // when the reference is scaled. Not gated here because an 8x8
            // leaf under a scaled reference is refused before this function
            // runs (see the block8 refusal below, ~12500) -- whoever lifts
            // that refusal must add `&& !ref_is_scaled` here too.
            // lane-fimv r1: libaom `motion_mode_allowed` also requires
            // `!xd->cur_frame_force_integer_mv`, so a screen-content frame
            // that set `force_integer_mv` reads the 2-symbol `obmc_cdf` here
            // (class wrong-alphabet-same-value).
            let proj_ok = num_proj_ref(
                grid,
                mi_row,
                mi_col,
                2,
                2,
                mi_cols as usize,
                mi_rows as usize,
                ref_frame,
                fctx,
            ) >= 1;
            // lane-t900 r28: the `&& !ref_is_scaled` the note above asked the
            // lifter of the 8x8-leaf refusal to add. libaom
            // `motion_mode_allowed` (blockd.h) makes WARPED_CAUSAL eligible
            // only `if (mbmi->num_proj_ref >= 1 && (allow_warped_motion &&
            // !av1_is_scaled(xd->block_ref_scale_factors[0])))`, so under a
            // scaled reference the ALPHABET shrinks to the 2-symbol obmc one
            // -- and with OBMC off no symbol is read at all (class
            // wrong-alphabet-same-value: reading the 3-symbol row here
            // desynced every warped-motion superres stream at its first
            // eligible 8x8 leaf).
            let ref_is_scaled = !scale_of((ref_width, ref_height), frame_size).is_unscaled();
            let warp_eligible =
                allow_warped_motion && !ref_is_scaled && !force_integer_mv && proj_ok;
            if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
                eprintln!(
                    "EC_MM8 mi_row={mi_row} mi_col={mi_col} proj={} warp_elig={} awm={}",
                    proj_ok as u8, warp_eligible as u8, allow_warped_motion as u8
                );
            }
            if allow_warped_motion && proj_ok && ref_is_scaled {
                hit!(SCALED_WARP_SUPPRESSED_HITS);
            }
            if allow_warped_motion && proj_ok && !ref_is_scaled && force_integer_mv {
                hit!(FIMV_ALPHABET_HITS);
            }
            if warp_eligible {
                crate::msac::SymbolDecoder::set_symr_cdf("motion_mode");
                let mode = dec.symbol(&mut cdfs.motion_mode[0]);
                match mode {
                    0 => {}
                    1 => {
                        obmc_selected = true;
                        hit!(OBMC_HITS);
                        hit!(OBMC_HITS_8);
                    }
                    _ => {
                        // lane-gmaffine r1: WARPED_CAUSAL at the 8x8 leaf --
                        // the same findSamples -> select_samples ->
                        // av1_find_projection chain the 16x16+ leaf runs,
                        // with BLOCK_8X8's own 8x8 model centre; an
                        // unusable projection falls back to the block's
                        // translational mv (libaom `av1_find_projection`
                        // returning 1 leaves `wm_params` invalid and
                        // `build_inter_predictors` skips the warp).
                        hit!(WARP_SELECTED_HITS);
                        let mut samples = find_samples(
                            grid,
                            mi_row,
                            mi_col,
                            2,
                            2,
                            mi_cols as usize,
                            mi_rows as usize,
                            ref_frame,
                            fctx,
                        );
                        if samples.len() > 1 {
                            crate::warp::select_samples(mv, &mut samples, SIDE as i32, SIDE as i32);
                        }
                        warp_params = crate::warp::find_projection(
                            &samples,
                            SIDE as i32,
                            SIDE as i32,
                            mv.1,
                            mv.0,
                            mi_row as i32,
                            mi_col as i32,
                        );
                        warped_selected = true;
                        if warp_params.is_some() {
                            hit!(WARP_HITS_8);
                        }
                    }
                }
            } else {
                // `default_obmc_cdf`'s own index: square bsize 8/16/32/64 -> 0/1/2/3.
                obmc_selected = dec.symbol(&mut cdfs.obmc[0]) == 1;
                if obmc_selected {
                    hit!(OBMC_HITS);
                    hit!(OBMC_HITS_8);
                }
            }
        }
        // lane-gmaffine r1: `read_mb_interp_filter` (spec 5.11.26) at the 8x8
        // leaf, in libaom's own order (interintra, motion_mode, THEN the
        // filter). Until this round the leaf read no filter symbol at all and
        // predicted Regular unconditionally -- correct only for a frame with a
        // fixed `interp_filter`, and a silent entropy DESYNC for every
        // SWITCHABLE-filter stream that reaches an 8x8 leaf (which is what
        // made the interior-split wiring above unusable at first).
        // corner-cut: the neighbour filter context is the enclosing 16x16
        // block's own coarse `above_filter`/`left_filter` entry (this leaf
        // path keeps no per-leaf neighbour array), so all four siblings read
        // the same context row. Ceiling: a stream whose 8x8 siblings pick
        // DIFFERENT filters decodes the second..fourth leaf's symbol from the
        // wrong CDF row. Upgrade: give `Neighbours` a real 8x8-granular
        // filter array, same shape as the `[3, 3]` approximation the caller
        // records after the leaf loop.
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
        let gm_nontrans = is_globalmv && gm_ref.model != ec_av1_syntax::WarpModel::Translation;
        let (h_filter, v_filter, resolved_filter) = resolve_interp_filter(
            dec,
            cdfs,
            interp_fixed,
            enable_dual_filter,
            gm_nontrans || warped_selected || skip_mode,
            above_filter_ctx,
            left_filter_ctx,
            false,
        );
        leaf_filter_syms = resolved_filter;
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_MODE_VAL8 mi_row={} mi_col={} newmv={is_new_mv} globalmv={is_globalmv} ref0={ref_frame} mv0=({},{}) stack={} rng={}",
                leaf_mi.0,
                leaf_mi.1,
                mv.0,
                mv.1,
                stack.entries.len(),
                dec.debug_state().0
            );
        }
        // lane-gmaffine r1: `allow_warp`'s `global_warp_allowed` branch
        // (`reconinter.c:33-55`), gated INDEPENDENTLY of `motion_mode` --
        // see [`decode_inter_block`]'s own copy. A local WARPED_CAUSAL model
        // wins (`is_none()` guard); BLOCK_8X8 already satisfies the >=8px
        // size predicate baked into `is_global_mv_block`.
        if warp_params.is_none() && is_global_mv_block && !force_integer_mv && !gm_ref.invalid {
            warp_params = crate::warp::global_warp_params(gm_ref.params);
            if warp_params.is_some() && gm_ref.model == ec_av1_syntax::WarpModel::Affine {
                hit!(AFFINE_GM_HITS);
            }
        }
        // lane-av112bitw: same lift as the 16x16+ leaf -- a local
        // `WARPED_CAUSAL` projection or a global model both leave
        // `warp_params` set here; the 12-bit refusal that stood here is
        // lifted with the same witness (`warp::warp_round_0` moved the
        // filter's shifts; the same stream decodes byte-exact vs ffmpeg).
        if crate::envflags::env_flag!("EC_TRACE_MODE") {
            eprintln!(
                "EC_WARP8 mi_row={} mi_col={} warp={} globalblk={is_global_mv_block}",
                leaf_mi.0,
                leaf_mi.1,
                warp_params.is_some(),
            );
        }
        grid.fill_rect(
            mi_row,
            mi_col,
            2,
            2,
            MiInfo {
                is_inter: true,
                ref_frame,
                // INTRA_FRAME marker for interintra blocks -- keeps
                // them out of warp-sample gathering (see 16/32 site).
                ref_frame1: if interintra_mode.is_some() {
                    0
                } else {
                    NO_REF1
                },
                mv1: (0, 0),
                mv: mv16(mv),
                is_new_mv,
                size: 2,
                size_h: 2,
                is_global_mv0: is_global_mv_block,
                is_global_mv1: false,
            },
        );
        mode_for_tx = 0;

        // lane-scaledref r1: this leaf's own prediction is always `Regular`
        // (documented corner-cut above); under a scaled reference the same
        // fixed kernel runs through spec 7.11.3.3's scaled walk instead.
        let luma_scale = scale_of((ref_width, ref_height), frame_size);
        if !luma_scale.is_unscaled() {
            hit!(SCALED_BLOCK8_HITS);
        }
        let obmc = if obmc_selected {
            Some(obmc_plan(
                grid,
                &neighbours.above_filter,
                &neighbours.left_filter,
                mi_row,
                mi_col,
                2,
                2,
                mi_rows as usize,
                mi_cols as usize,
                SIDE,
                SIDE,
                SIDE,
                ss_x(fctx),
                ss_y(fctx),
                chroma_w,
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
        // lane-defer8: this leaf's whole prediction as a closure, run at the
        // push site like [`decode_inter_block`]'s.
        let build = move |y: &mut PlaneBuf<'static>,
                          u: &mut PlaneBuf<'static>,
                          v: &mut PlaneBuf<'static>,
                          refpix: &RefPix<'_>,
                          fctx: &crate::decode::FrameCtx,
                          out: &mut [Vec<u16>; 3]| {
            let Some((sref_y, sref_u, sref_v)) = ref_planes_deferred(ref_frame, refpix) else {
                return;
            };
            let mut pred_y = refill(std::mem::take(&mut out[0]), SIDE * SIDE);
            let mut pred_u = refill(std::mem::take(&mut out[1]), chroma_w * chroma_h);
            let mut pred_v = refill(std::mem::take(&mut out[2]), chroma_w * chroma_h);
            // Merge of lane-gmaffine (this leaf's own switchable filters) with
            // lane-scaledref (spec 7.11.3.3's scaled walk): same kernels either
            // way, only the sample walk differs.
            for (plane, x, y, (dim_w, dim_h), dst) in [
                (sref_y, px, py, (SIDE, SIDE), &mut pred_y),
                (sref_u, cpx, cpy, (chroma_w, chroma_h), &mut pred_u),
                (sref_v, cpx, cpy, (chroma_w, chroma_h), &mut pred_v),
            ] {
                let luma = std::ptr::eq(plane, sref_y);
                if luma_scale.is_unscaled() {
                    mc::predict_with_filters(
                        &plane.data,
                        plane.width,
                        plane.true_width,
                        plane.true_height,
                        mv_to_q4(x, mv.1, if luma { 0 } else { ss_x(fctx) }),
                        mv_to_q4(y, mv.0, if luma { 0 } else { ss_y(fctx) }),
                        dim_w,
                        dim_h,
                        h_filter,
                        v_filter,
                        dst,
                        fctx,
                    );
                } else {
                    mc::predict_scaled(
                        &plane.data,
                        plane.width,
                        plane.true_width,
                        plane.true_height,
                        mv_to_q4(x, mv.1, if luma { 0 } else { ss_x(fctx) }),
                        mv_to_q4(y, mv.0, if luma { 0 } else { ss_y(fctx) }),
                        luma_scale,
                        dim_w,
                        dim_h,
                        h_filter,
                        v_filter,
                        dst,
                        fctx,
                    );
                }
            }

            // lane-gmaffine r1: the warp filter REPLACES the translational
            // prediction just built (libaom builds one or the other), same as
            // the 16x16+ leaf's own call trio.
            // lane-t900 r28: `allow_warp`'s scaled-reference bail-out, which this
            // leaf was missing -- it is the defect the 8x8-leaf refusal hid (a
            // warped 8x8 leaf over a superres reference; libaom predicts it
            // translationally, every symbol still read).
            if let Some(params) = warp_params.as_ref().filter(|_| luma_scale.is_unscaled()) {
                crate::warp::warp_affine(
                    params,
                    &sref_y.data,
                    sref_y.true_width as i32,
                    sref_y.true_height as i32,
                    sref_y.width as i32,
                    &mut pred_y,
                    px as i32,
                    py as i32,
                    SIDE as i32,
                    SIDE as i32,
                    SIDE as i32,
                    0,
                    0,
                    fctx,
                );
                // `av1_init_warp_params`'s per-plane bail-out is on BOTH
                // extents (`block_height < 8 || block_width < 8`,
                // reconinter.c:61): an 8x8 leaf's chroma plane block is under 8
                // at any subsampled format (4:2:0 4x4, 4:2:2 4x8), so it is
                // predicted translationally even when the luma warps; 4:4:4's
                // 8x8 chroma warps with the luma, and a rect chroma block 8 wide
                // but under 8 high (4:2:2 BLOCK_8X4) stays translational too.
                // lane-av1444altref r3: this arm tested only the width.
                if warp_plane_allowed(chroma_w, chroma_h) {
                    crate::warp::warp_affine(
                        params,
                        &sref_u.data,
                        sref_u.true_width as i32,
                        sref_u.true_height as i32,
                        sref_u.width as i32,
                        &mut pred_u,
                        cpx as i32,
                        cpy as i32,
                        chroma_w as i32,
                        chroma_h as i32,
                        chroma_w as i32,
                        ss_x(fctx) as i32,
                        ss_y(fctx) as i32,
                        fctx,
                    );
                    crate::warp::warp_affine(
                        params,
                        &sref_v.data,
                        sref_v.true_width as i32,
                        sref_v.true_height as i32,
                        sref_v.width as i32,
                        &mut pred_v,
                        cpx as i32,
                        cpy as i32,
                        chroma_w as i32,
                        chroma_h as i32,
                        chroma_w as i32,
                        ss_x(fctx) as i32,
                        ss_y(fctx) as i32,
                        fctx,
                    );
                }
            }

            if let Some(plan) = &obmc {
                obmc_run(plan, refpix, &mut pred_y, &mut pred_u, &mut pred_v, fctx);
            }

            // Non-wedge interintra blend, mutually exclusive with OBMC (the
            // motion_mode symbol is not read for an interintra block).
            if let Some(ii) = interintra_mode {
                interintra_blend(
                    y,
                    px,
                    py,
                    SIDE,
                    SIDE,
                    SIDE,
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
                    chroma_w,
                    chroma_h,
                    chroma_w,
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
                    chroma_w,
                    chroma_h,
                    chroma_w,
                    ss_x(fctx),
                    ss_y(fctx),
                    ii,
                    wedge_mask,
                    &mut pred_v,
                    fctx,
                );
            }

            *out = [pred_y, pred_u, pred_v];
        };
        let (pred_y, pred_u, pred_v): (Vec<u16>, Vec<u16>, Vec<u16>);
        let (sy, su, sv) = if pred_deferred(refpix) {
            push_pred(build);
            (Pred::Cur { plane: 0, stride: SIDE, off: 0 }, Pred::Cur { plane: 1, stride: chroma_w, off: 0 }, Pred::Cur { plane: 2, stride: chroma_w, off: 0 })
        } else {
            // The interintra predictor reads this block's own above/left
            // edges, which are already written at this point.
            let mut built: [Vec<u16>; 3] = Default::default();
            build(y, u, v, refpix, fctx, &mut built);
            let [by, bu, bv] = built;
            (pred_y, pred_u, pred_v) = (by, bu, bv);
            (
                Pred::Inline(&pred_y, SIDE),
                Pred::Inline(&pred_u, chroma_w),
                Pred::Inline(&pred_v, chroma_w),
            )
        };
        split8 = read_block_tx_size(
            dec,
            cdfs,
            neighbours,
            leaf_mi,
            SIDE,
            (mi_cols as usize, mi_rows as usize),
            true,
            skip,
            fctx,
        )?
        .1
        .is_some();
        if skip {
            push_mc(
                y,
                u,
                v,
                0,
                px,
                py,
                SIDE,
                sy,
                &ZERO_RESIDUAL[..SIDE * SIDE],
                fctx,
            );
            // lane-av1-422sub8rect: the plane block is the (chroma_w,
            // chroma_h) rect at 4:2:2 -- the recorded write is stride
            // `chroma_w`, full `chroma_h` rows.
            push_mc_rect(
                y,
                u,
                v,
                1,
                cpx,
                cpy,
                chroma_w,
                chroma_w,
                chroma_h,
                su,
                &ZERO_RESIDUAL[..chroma_w * chroma_h],
                fctx,
            );
            push_mc_rect(
                y,
                u,
                v,
                2,
                cpx,
                cpy,
                chroma_w,
                chroma_w,
                chroma_h,
                sv,
                &ZERO_RESIDUAL[..chroma_w * chroma_h],
                fctx,
            );
            luma_grid = Grid::Zero(SIDE * SIDE);
            u_grid = Grid::Zero(chroma_w * chroma_h);
            v_grid = Grid::Zero(chroma_w * chroma_h);
        } else {
            let around = neighbours.around_mi(leaf_mi, 8);
            // lane-av1-422sub8rect: the 4:2:2 chroma unit is the TX_4X8
            // rect -- its `txb_skip_ctx` reads per chroma 4-px cell
            // ([`Neighbours::around_mi_422_chroma`], the key-frame
            // BLOCK_4X8 arm's own context walk), and the coefficients go
            // through the inter rect reader with the inherited luma tx
            // type.
            let chroma_around = if chroma_422 {
                neighbours.around_mi_422_chroma(leaf_mi, 8, 8)
            } else {
                around
            };
            let (grid8, luma_tx_type) = read_inter_luma8(
                y,
                u,
                v,
                dec,
                cdfs,
                neighbours,
                leaf_mi,
                px,
                py,
                sy,
                mode_for_tx,
                base_q_idx,
                scan8,
                scan4,
                reduced_tx_set,
                split8,
                fctx,
            )?;
            luma_grid = grid8;
            // Lossless 8x8 inter leaf, compound twin of the single-ref arm
            // above -- same per-axis plane block (4:4:4 8x8, 4:2:2 4x8) and
            // the same "bigger than one TX_4X4" gate.
            if lossless(fctx) && (chroma_w > 4 || chroma_h > 4) && !mono(fctx) {
                let (ug, vg) = leaf8_inter_chroma_lossless(
                    y,
                    u,
                    v,
                    dec,
                    cdfs,
                    neighbours,
                    su,
                    sv,
                    leaf_mi,
                    (cpx, cpy),
                    (chroma_w, chroma_h),
                    chroma_w,
                    base_q_idx,
                    fctx,
                )?;
                u_grid = ug;
                v_grid = vg;
                chroma_stamped_per_unit = true;
            } else {
                u_grid = if chroma_422 {
                    read_inter_plane_rect(
                        dec,
                        cdfs,
                        chroma_set,
                        (chroma_w, chroma_h),
                        chroma_w,
                        1,
                        chroma_around[1],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        su,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0
                } else {
                    read_inter_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        chroma_scan,
                        1,
                        chroma_around[1],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_w,
                        base_q_idx,
                        su,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0
                };
                v_grid = if chroma_422 {
                    read_inter_plane_rect(
                        dec,
                        cdfs,
                        chroma_set,
                        (chroma_w, chroma_h),
                        chroma_w,
                        2,
                        chroma_around[2],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        sv,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0
                } else {
                    read_inter_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        chroma_scan,
                        2,
                        chroma_around[2],
                        mode_for_tx,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_w,
                        base_q_idx,
                        sv,
                        Some(luma_tx_type),
                        None,
                        fctx,
                    )?
                    .0
                };
            }
        }
    } else {
        crate::msac::SymbolDecoder::set_symr_cdf("y_mode");
        let mode = dec.symbol(&mut cdfs.y_mode[SIZE_GROUP_8]);
        if mode >= 13 {
            return Err(unsupported(
                "an intra mode this decoder does not code (round 2)",
            ));
        }
        // lane-angleinter r1: `read_intra_angle_info` (libaom `decodemv.c`,
        // inside `read_intra_block_mode_info`) -- an intra block in an inter
        // frame reads `angle_delta_y` off the same
        // `angle_delta_cdf[mode - V_PRED]` row a key-frame block does, and
        // the delta feeds the directional predictor (and the edge
        // filter/upsample decision derived from the delta'd angle) unchanged.
        // The symbol was read and then refused before this round even though
        // aomenc enables `--enable-angle-delta` by default.
        let angle_delta_y = if (V_PRED..=D67_PRED).contains(&mode) {
            read_angle_delta(dec, &mut cdfs.angle_delta[mode - V_PRED])
        } else {
            0
        };
        if angle_delta_y != 0 {
            hit!(INTRA_IN_INTER_ANGLE_DELTA_Y_HITS);
            // lane-leaf8tx r1: `av1_use_angle_delta(BLOCK_8X8)` is true
            // (`bsize >= BLOCK_8X8`), so this leaf codes `angle_delta_y` off
            // `angle_delta_cdf[mode - V_PRED]` exactly as a key-frame block
            // does (libaom `read_intra_angle_info` inside
            // `read_intra_block_mode_info`, decodemv.c), and the delta feeds
            // the directional predictor + the edge filter/upsample decision
            // through the same `reconstruct` argument below. Counted
            // separately from the >=16x16 arm so the gate cannot be satisfied
            // by a bigger block.
            hit!(INTRA_IN_INTER8_ANGLE_DELTA_Y_HITS);
        }
        // lane-uv8 r1: the delta'd angle decides the intra edge filter, and a
        // directional luma mode is already legal on this leaf -- the arm
        // passed a hardcoded `false` where [`decode_leaf8`] reads the
        // mi-exact neighbour modes (`get_intra_edge_filter_type`, spec
        // 7.11.2).
        let (above_mode, left_mode) =
            neighbours.modes_above_left_mi(leaf_mi.0, leaf_mi.1, neighbours.modes_above_left(r, c));
        let smooth_neighbor = is_smooth_mode(above_mode) || is_smooth_mode(left_mode);
        // lane-uv8 r1: libaom `read_intra_block_mode_info` (decodemv.c:1198-1207)
        // -- `uv_mode` over the CFL-allowed alphabet (`is_cfl_allowed`: a
        // BLOCK_8X8 leaf is inside the 32x32 bound), then `read_cfl_alphas`
        // for `UV_CFL_PRED`, then `angle_delta_uv` off the MAPPED uv mode's
        // row (`get_uv_mode`, identity over `V_PRED..=D67_PRED`). Every value
        // but `DC_PRED` was refused here, which is what blocked the low-cq
        // CDEF arms and the 10-bit gates.
        let has_chroma = !mono(fctx);
        // lane-av1-llintra8: `read_intra_mode_uv` (decodemv.c:145) picks the
        // uv CDF row by `is_cfl_allowed` (cfl.h:23) -- at LOSSLESS that is
        // "the chroma plane block is BLOCK_4X4" (cfl.h:29), for this
        // BLOCK_8X8 leaf only at 4:2:0. The 4:4:4 lossless leaf reads the
        // 13-symbol `uv_mode_no_cfl` alphabet; the unconditional CFL row
        // consumed a different partition than aomdec at the leaf's own
        // uv_mode symbol (class wrong-alphabet-same-value) and desynced
        // every stream that reached the leaf's lossless chroma walk below.
        // 4:2:0 and every non-lossless partition keep the CFL row
        // (`cfl_allowed_px` answers true exactly as before).
        let uv_mode = if !has_chroma {
            DC_PRED
        } else if cfl_allowed_px(SIDE, SIDE, fctx) {
            crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
            dec.symbol(&mut cdfs.uv_mode_cfl[mode])
        } else {
            crate::msac::SymbolDecoder::set_symr_cdf("uv_mode");
            dec.symbol(&mut cdfs.uv_mode_no_cfl[mode])
        };
        if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
            eprintln!(
                "EC_ISTEP8 mi_row={} mi_col={} name=modes y={mode} uv={uv_mode} fi_enabled={} rng={}",
                leaf_mi.0,
                leaf_mi.1,
                fctx.enable_filter_intra_inter.with(std::cell::Cell::get),
                dec.debug_state().0
            );
        }
        if (9..=12).contains(&uv_mode) {
            hit!(SMOOTH_UV_HITS);
            hit!(INTRA_IN_INTER8_UV_SMOOTH_HITS);
        }
        let alpha = if has_chroma && uv_mode == UV_CFL_PRED {
            hit!(INTRA_IN_INTER8_UV_CFL_HITS);
            Some(read_cfl_alphas(dec, cdfs))
        } else {
            None
        };
        let angle_delta_uv = if (V_PRED..=D67_PRED).contains(&uv_mode) {
            hit!(DIRECTIONAL_UV_HITS);
            hit!(INTRA_IN_INTER8_UV_DIR_HITS);
            read_angle_delta(dec, &mut cdfs.angle_delta[uv_mode - V_PRED])
        } else {
            0
        };
        if angle_delta_uv != 0 {
            hit!(UV_ANGLE_DELTA_HITS);
            hit!(INTRA_IN_INTER_ANGLE_DELTA_UV_HITS);
        }
        // `get_uv_mode` (spec 9.3): `UV_CFL_PRED` predicts as `DC_PRED`.
        uv_predict_mode = if uv_mode == UV_CFL_PRED {
            DC_PRED
        } else {
            uv_mode
        };
        // The CHROMA neighbour's own `uv_mode` decides the chroma edge filter
        // strength (`get_intra_edge_filter_type`, reconintra.c:974), never the
        // luma one -- same read [`decode_leaf8`] does.
        let smooth_neighbor_uv = neighbours.smooth_uv_neighbour(leaf_mi.0, leaf_mi.1, r, c, fctx);
        // lane-screen r2: see [`decode_inter_block`]'s own copy -- `mode`/
        // `uv_mode` are both effectively `DC_PRED`-gated already on this
        // leaf (a nonzero `mode` still passes, but `uv_mode` is forced
        // `DC_PRED` by the refusal just above), same order as libaom's
        // `read_palette_mode_info` call right after `xd->cfl.store_y`.
        // lane-t900 r23: reconstructed, not refused. Same syntax, same
        // deferral of the colour-index maps past `use_filter_intra`
        // (`av1_visit_palette` runs from `parse_decode_block`, after the whole
        // mode-info read) as the >=16 intra-in-inter block's own copy in
        // [`decode_inter_block`]; the mode context, the colour cache and the
        // `palette_uv_mode` context are this leaf's REAL ones, not the
        // hardcoded 0/empty pair the refusal could get away with while no
        // neighbour could ever be a palette (class
        // [[cdf-row-held-constant]]). `av1_get_palette_bsize_ctx(BLOCK_8X8)`
        // = `num_pels_log2_lookup[BLOCK_8X8] - 6` = row 0.
        let (palette_ctx, palette_cache) = neighbours.palette_ctx_and_cache_mi(leaf_mi);
        let palette_uv_cache = neighbours.palette_uv_cache_mi(leaf_mi);
        let mut palette_y_pending: Option<(usize, [u16; 8])> = None;
        let mut palette_uv_pending: Option<(usize, [u16; 8], [u16; 8])> = None;
        if allow_screen_content_tools && let Some(bsize_ctx) = palette_bsize_ctx(SIDE) {
            let use_palette_y = mode == DC_PRED
                && dec.symbol(&mut cdfs.palette_y_mode[bsize_ctx][palette_ctx]) != 0;
            if use_palette_y {
                let n = 2 + dec.symbol(&mut cdfs.palette_y_size[bsize_ctx]);
                palette_y_pending = Some((n, read_palette_colors_y(dec, n, &palette_cache, fctx)));
            }
            // lane-uv8 r1: `read_palette_mode_info` reads the UV symbol only
            // when `uv_mode == UV_DC_PRED` (decodemv.c).
            let use_palette_uv = has_chroma
                && uv_mode == DC_PRED
                && dec.symbol(&mut cdfs.palette_uv_mode[usize::from(use_palette_y)]) != 0;
            if crate::envflags::env_flag!("EC_PALSYN") {
                eprintln!(
                    "EC_PALSYN leaf8 mi={},{} bw=8 bh=8 mode={mode} uv={uv_mode} ctx={palette_ctx} y={} uv={} rng={}",
                    leaf_mi.0,
                    leaf_mi.1,
                    palette_y_pending.map_or(0, |(n, _)| n),
                    u8::from(use_palette_uv),
                    dec.debug_state().0
                );
            }
            if use_palette_uv {
                let n = 2 + dec.symbol(&mut cdfs.palette_uv_size[bsize_ctx]);
                let (u_colors, v_colors) = read_palette_colors_uv(dec, n, &palette_uv_cache, fctx);
                palette_uv_pending = Some((n, u_colors, v_colors));
            }
        }
        // lane-fiinter r1: `read_filter_intra_mode_info` (libaom `decodemv.c`,
        // the tail of `read_intra_block_mode_info`) -- an intra block inside
        // an INTER frame reads `use_filter_intra` in exactly the same place a
        // key-frame block does, right after the palette syntax. Nothing read
        // it here before this round, and aomenc enables filter intra by
        // default, so every inter frame carrying one desynced at this symbol.
        // `av1_filter_intra_allowed`'s `palette_size[0] == 0` term is implied:
        // a real palette-Y block already returned above.
        let mut filter_intra = None;
        // lane-uv8 r2: the oracle's `EC_ISTEP` ladder (`EC_TRACE_MODE_STEP`)
        // prints `use_filter_intra` for an intra block inside an INTER frame
        // and nothing else, so this leaf needs the same two lines for a
        // range-ladder diff against it.
        let ec_istep8 = crate::envflags::env_flag!("EC_TRACE_MODE_STEP");
        if mode == DC_PRED
            && fctx.enable_filter_intra_inter.with(std::cell::Cell::get)
            // `av1_filter_intra_allowed` (reconintra.h:77) excludes a
            // palette-Y block, so no `use_filter_intra` symbol exists there.
            && palette_y_pending.is_none()
            && let Some(class) = filter_intra_size_class(SIDE)
        {
            let use_fi = dec.symbol(&mut cdfs.filter_intra[class]) != 0;
            if ec_istep8 {
                eprintln!(
                    "EC_ISTEP mi_row={} mi_col={} name=use_filter_intra val={} rng={}",
                    leaf_mi.0,
                    leaf_mi.1,
                    u8::from(use_fi),
                    dec.debug_state().0
                );
            }
            if use_fi {
                hit!(FILTER_INTRA_HITS);
                hit!(INTRA_IN_INTER_FILTER_INTRA_HITS);
                let fi = dec.symbol(&mut cdfs.filter_intra_mode);
                if ec_istep8 {
                    eprintln!(
                        "EC_ISTEP mi_row={} mi_col={} name=filter_intra_mode val={fi} rng={}",
                        leaf_mi.0,
                        leaf_mi.1,
                        dec.debug_state().0
                    );
                }
                filter_intra = Some(fi);
            }
        }
        // lane-t900 r23: `av1_visit_palette` -- the two colour-index maps, Y
        // then the shared chroma one, after the WHOLE mode-info read.
        let palette_y = palette_y_pending.map(|(n, colors)| {
            let map = decode_color_index_map_wh(
                dec,
                cdfs,
                n,
                SIDE,
                SIDE,
                palette_onscreen(rmi, cmi, SIDE, SIDE, fctx),
                false,
            );
            hit!(PALETTE_HITS);
            hit!(INTRA_IN_INTER_PALETTE_Y_HITS);
            hit_do! {
                INTER8_PALETTE_HITS.with(|c| {
                    let mut v = c.get();
                    v.0 += 1;
                    c.set(v);
                });
            }
            PaletteY {
                size: n,
                colors,
                map,
            }
        });
        let palette_uv = palette_uv_pending.map(|(n, u_colors, v_colors)| {
            // `av1_get_plane_block_size(BLOCK_8X8, ss)` at the sequence's own
            // subsampling: BLOCK_4X4 at 4:2:0, BLOCK_8X8 at 4:4:4, and at
            // 4:2:2 the BLOCK_4X8 plane block (lane-av1-422sub8rect).
            let on = palette_onscreen_uv(
                (SIDE, SIDE),
                palette_onscreen(rmi, cmi, SIDE, SIDE, fctx),
                (chroma_w, chroma_h),
                (ss_x(fctx), ss_y(fctx)),
            );
            let map = decode_color_index_map_wh(dec, cdfs, n, chroma_w, chroma_h, on, true);
            hit!(PALETTE_UV_HITS);
            hit!(INTRA_IN_INTER_PALETTE_UV_HITS);
            hit_do! {
                INTER8_PALETTE_HITS.with(|c| {
                    let mut v = c.get();
                    v.1 += 1;
                    c.set(v);
                });
            }
            PaletteUv {
                size: n,
                u_colors,
                v_colors,
                map,
            }
        });
        palette_record8 = (
            palette_y.as_ref().map_or(0, |p| p.size),
            palette_y.as_ref().map_or([0u16; 8], |p| p.colors),
            palette_uv.as_ref().map_or(0, |p| p.size),
            palette_uv.as_ref().map_or([0u16; 8], |p| p.u_colors),
        );
        // A palette block predicts from its own colour-index map, never from
        // the block edge (spec 7.11.2/7.11.4) -- these ride the
        // [`PALETTE_PRED`] slot into each `reconstruct`/`read_plane` call
        // below, exactly as the key-frame and >=16 intra-in-inter paths do.
        let palette_y_buf: Option<Vec<u16>> = palette_y
            .as_ref()
            .map(|p| p.map.iter().map(|&i| p.colors[i as usize]).collect());
        let palette_uv_bufs: Option<(Vec<u16>, Vec<u16>)> = palette_uv.as_ref().map(|p| {
            (
                p.map.iter().map(|&i| p.u_colors[i as usize]).collect(),
                p.map.iter().map(|&i| p.v_colors[i as usize]).collect(),
            )
        });
        mode_for_tx = mode;
        let (mi_row, mi_col) = leaf_mi;
        grid.fill_rect(
            mi_row,
            mi_col,
            2,
            2,
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

        // lane-leaf8tx r1: the 8x8 leaf's own intra `tx_depth` (libaom
        // `read_tx_size` -> `read_selected_tx_size`, max depth 1 at
        // BLOCK_8X8) resolves to TX_4X4, and `decode_block`
        // (`decodeframe.c`) then codes one intra prediction + one
        // coefficient set PER 4x4 transform unit in raster order, each unit
        // predicting off the units reconstructed before it -- the same loop
        // the >=16x16 intra-in-inter arm runs. Chroma stays one 4x4 unit
        // (`av1_get_max_uv_txsize`).
        let intra_leaves = read_block_tx_size(
            dec,
            cdfs,
            neighbours,
            leaf_mi,
            SIDE,
            (mi_cols as usize, mi_rows as usize),
            false,
            skip,
            fctx,
        )?
        .1;
        // lane-leaf8tx r4: the per-TU loop below is entropy- and pixel-exact
        // against aomdec once `tx_size_context_txfm` stopped reading
        // `above_inter`/`left_inter` four mi away (r3), so the refusal that
        // used to sit here is lifted; gate
        // `a_real_aomenc_{,10bit_}inter_sequence_with_an_angle_delta_8x8_intra_leaf_decodes_pixel_exact`.
        split8 = intra_leaves.is_some();
        let reach = Reach::of(SIDE, px, py, y.width, y.height, fctx);
        if let Some(leaves) = intra_leaves {
            for &(row, col, tx_px, _) in &leaves {
                let tu_mi = (leaf_mi.0 + row, leaf_mi.1 + col);
                let (tu_px, tu_py) = (px + col * MI, py + row * MI);
                // A transform unit inside a block is not a block at that
                // position: top-right / bottom-left availability is the
                // block's, narrowed by the unit's offset inside it.
                let tu_reach = Reach::of_tu(SIDE, SIDE, col * MI, row * MI, tx_px, tx_px, reach);
                // This unit's own window on the block's palette prediction
                // (the whole-block buffer would be indexed at the wrong
                // stride by the first unit).
                if let Some(buf) = &palette_y_buf {
                    hit!(PALETTE_SPLIT_TX_HITS);
                    let window: Vec<u16> = (0..tx_px)
                        .flat_map(|i| {
                            let base = (row * MI + i) * SIDE + col * MI;
                            buf[base..base + tx_px].iter().copied()
                        })
                        .collect();
                    set_palette_pred(window, fctx);
                }
                let tu_grid = if skip {
                    push_intra(
                        y,
                        u,
                        v,
                        0,
                        tu_px,
                        tu_py,
                        tx_px,
                        mode,
                        angle_delta_y,
                        tu_reach,
                        &ZERO_RESIDUAL[..tx_px * tx_px],
                        None,
                        filter_intra,
                        smooth_neighbor,
                        fctx,
                    );
                    Grid::Zero(tx_px * tx_px)
                } else {
                    // The unit is smaller than the block it sits in, so
                    // `txb_skip_ctx` is the neighbour-magnitude table, not
                    // the lone-TU 0 (`get_txb_ctx_general`).
                    let tu_skip_ctx = neighbours.luma_skip_ctx(tu_mi, tx_px / MI);
                    read_plane(
                        dec,
                        cdfs,
                        // Intra 4x4 luma: seven-type
                        // `EXT_TX_SET_DTT4_IDTX_1DDCT` unless reduced, whose
                        // CDF row is the intra dir (`fi_tx_row` inside
                        // `read_plane` maps a filter-intra block's).
                        txbset_for(tx_px, reduced_tx_set),
                        default_scan(tx_px),
                        0,
                        neighbours.around_mi(tu_mi, tx_px)[0],
                        mode,
                        mode,
                        angle_delta_y,
                        tu_reach,
                        y,
                        u,
                        v,
                        tu_px,
                        tu_py,
                        tx_px,
                        tx_px,
                        base_q_idx,
                        None,
                        filter_intra,
                        Some(tu_skip_ctx),
                        smooth_neighbor,
                        fctx,
                    )?
                };
                neighbours.record_mi_luma(tu_mi, tx_px, &tu_grid);
            }
        }
        if skip {
            if !split8 {
                if let Some(buf) = &palette_y_buf {
                    set_palette_pred(buf.clone(), fctx);
                }
                push_intra(
                    y,
                    u,
                    v,
                    0,
                    px,
                    py,
                    SIDE,
                    mode,
                    angle_delta_y,
                    reach,
                    &ZERO_RESIDUAL[..SIDE * SIDE],
                    None,
                    filter_intra,
                    smooth_neighbor,
                    fctx,
                );
            }
            // CfL's AC contribution is the luma this leaf just reconstructed,
            // averaged over the 4x4 chroma block (`cfl_ac_q3`, spec 7.11.5).
            let ac = alpha.map(|_| cfl_src(px, py, SIDE));
            if let Some((ub, _)) = &palette_uv_bufs {
                set_palette_pred(ub.clone(), fctx);
            }
            // lane-av1-422sub8rect: the plane block is the (chroma_w,
            // chroma_h) rect at 4:2:2 -- the recorded prediction/write uses
            // the rect push.
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
            luma_grid = Grid::Zero(SIDE * SIDE);
            u_grid = Grid::Zero(chroma_w * chroma_h);
            v_grid = Grid::Zero(chroma_w * chroma_h);
        } else {
            let around = neighbours.around_mi(leaf_mi, 8);
            luma_grid = if split8 {
                // Already coded and reconstructed unit by unit above; the
                // block-level grid only feeds the neighbour write-back,
                // which the `saved_luma_ctx` restore below keeps per-TU.
                Grid::Zero(SIDE * SIDE)
            } else {
                if let Some(buf) = &palette_y_buf {
                    set_palette_pred(buf.clone(), fctx);
                }
                read_plane(
                    dec,
                    cdfs,
                    // lane-uv8 r3: an intra leaf inside an inter frame is
                    // `is_inter == 0` in `av1_get_ext_tx_set_type`, so its 8x8
                    // luma reads the INTRA set -- seven-type
                    // `EXT_TX_SET_DTT4_IDTX_1DDCT` when `reduced_tx_set == 0`,
                    // five-type `EXT_TX_SET_DTT4_IDTX` when it is 1. Hardcoding
                    // the reduced set read the wrong ALPHABET (class
                    // [[wrong-alphabet-same-value]]) and desynced at the leaf's
                    // first `tx_type`.
                    txbset_for(8, reduced_tx_set),
                    scan8,
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
                    SIDE,
                    TX8,
                    base_q_idx,
                    None,
                    filter_intra,
                    None,
                    smooth_neighbor,
                    fctx,
                )?
            };
            let ac = alpha.map(|_| cfl_src(px, py, SIDE));
            // lane-av1-422sub8rect: at 4:2:2 the plane block is the BLOCK_4X8
            // rect and the key-frame arm's own reader applies -- one
            // TX_4X8 `ChromaRect8x4` unit per plane through
            // [`read_rect_chroma_unit`] with the per-chroma-cell context
            // walk ([`Neighbours::around_mi_422_chroma`], the
            // lane-av1-422f model this leaf's key-frame twin proves
            // byte-exact). 4:2:0/4:4:4 keep the square [`read_plane`] read.
            let (chroma_around_u, chroma_around_v) = if chroma_422 {
                let ca = neighbours.around_mi_422_chroma(leaf_mi, 8, 8);
                (ca[1], ca[2])
            } else {
                (around[1], around[2])
            };
            // lane-av1-llintra8: lossless codes TX_4X4 on EVERY plane
            // (`av1_get_tx_size`, blockd.h:1383), so this intra leaf's chroma
            // plane block is a raster of TX_4X4 units per plane, plane-major
            // -- the same per-unit walk the >=16x16 intra-in-inter block
            // runs, not one TX_8X8 unit (whose WHT hits `TxParams::run`'s
            // `(8, 8) != (4, 4)` lossless assert). The plane block is
            // PER-AXIS, `av1_ss_size_lookup[BLOCK_8X8][ss_x][ss_y]`: 8x8 at
            // 4:4:4 (2x2 units) and 4x8 at 4:2:2 (1x2), so both walk here and
            // only 4:2:0's 4x4 keeps the single-unit read in the `else`
            // below (`plane_bsize == txsize_to_bsize[tx_size]`, ctx_offset 7).
            // The walk stamps each unit's own coefficient context
            // (`record_mi_chroma` inside it), so the tail's whole-block
            // `record_mi` gets its chroma half saved/restored around --
            // the same rule the INTER arms'
            // `leaf8_inter_chroma_lossless` obeys.
            if lossless(fctx) && (chroma_w > 4 || chroma_h > 4) && !mono(fctx) {
                let (mut uo, mut vo) = (Vec::new(), Vec::new());
                read_intra_chroma_lossless(
                    y,
                    u,
                    v,
                    dec,
                    cdfs,
                    neighbours,
                    leaf_mi,
                    (px, py),
                    (cpx, cpy),
                    SIDE,
                    (chroma_w, chroma_h),
                    (0, 0),
                    (chroma_w, chroma_h),
                    chroma_w,
                    reach,
                    (y.width, y.height),
                    mode,
                    uv_predict_mode,
                    angle_delta_uv,
                    palette_uv_bufs.as_ref().map(|(a, b)| (&a[..], &b[..])),
                    smooth_neighbor_uv,
                    base_q_idx,
                    &mut uo,
                    &mut vo,
                    fctx,
                )?;
                u_grid = if uo.is_empty() {
                    Grid::Zero(chroma_w * chroma_h)
                } else {
                    Grid::Own(uo)
                };
                v_grid = if vo.is_empty() {
                    Grid::Zero(chroma_w * chroma_h)
                } else {
                    Grid::Own(vo)
                };
                chroma_stamped_per_unit = true;
                hit!(LLINTRA8_CHROMA_WALK_HITS);
            } else {
                if let Some((ub, _)) = &palette_uv_bufs {
                    set_palette_pred(ub.clone(), fctx);
                }
                u_grid = if chroma_422 {
                    read_rect_chroma_unit(
                        dec,
                        cdfs,
                        chroma_set,
                        chroma_scan,
                        chroma_w,
                        chroma_h,
                        1,
                        chroma_around_u,
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
                    )?
                } else {
                    read_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        chroma_scan,
                        1,
                        chroma_around_u,
                        mode,
                        uv_predict_mode,
                        angle_delta_uv,
                        reach,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_w,
                        chroma_tx,
                        base_q_idx,
                        alpha.zip(ac).map(|((au, _), ac)| (au, ac)),
                        None,
                        None,
                        smooth_neighbor_uv,
                        fctx,
                    )?
                };
                if let Some((_, vb)) = &palette_uv_bufs {
                    set_palette_pred(vb.clone(), fctx);
                }
                v_grid = if chroma_422 {
                    read_rect_chroma_unit(
                        dec,
                        cdfs,
                        chroma_set,
                        chroma_scan,
                        chroma_w,
                        chroma_h,
                        2,
                        chroma_around_v,
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
                    )?
                } else {
                    read_plane(
                        dec,
                        cdfs,
                        chroma_set,
                        chroma_scan,
                        2,
                        chroma_around_v,
                        mode,
                        uv_predict_mode,
                        angle_delta_uv,
                        reach,
                        y,
                        u,
                        v,
                        cpx,
                        cpy,
                        chroma_w,
                        chroma_tx,
                        base_q_idx,
                        alpha.zip(ac).map(|((_, av), ac)| (av, ac)),
                        None,
                        None,
                        smooth_neighbor_uv,
                        fctx,
                    )?
                };
            }
        }
    }
    // lane-txselect: a split leaf's plane-0 neighbour context is per 4x4
    // transform unit (written by `record_mi_luma` inside `read_inter_luma8`),
    // but `record_mi` rewrites all three planes of these cells at once -- so
    // save plane 0 across it, exactly as `record_split_luma` leaves plane 0
    // alone at the bigger block sizes.
    // lane-lossless3: `!skip` too. A SKIP block codes no coefficients and so
    // publishes nothing per transform unit, and on a LOSSLESS frame
    // `read_block_tx_size` hands back 4x4 leaves for a skip block as well
    // (libaom's `read_tx_size` returns TX_4X4 before it reads anything), so
    // `split8` is true over an UNTOUCHED band -- the restore then put the
    // previous block's levels back over the zeros `record_mi` had just
    // written, where libaom runs `av1_reset_entropy_context`. Class
    // [[override-slot-on-one-arm]]: a save/restore is only valid on the arm
    // that actually wrote the per-TU state.
    let saved_luma_ctx = (split8 && !skip).then(|| {
        (
            [
                neighbours.left[leaf_mi.0][0],
                neighbours.left[leaf_mi.0 + 1][0],
            ],
            [
                neighbours.above[leaf_mi.1][0],
                neighbours.above[leaf_mi.1 + 1][0],
            ],
        )
    });
    // lane-t900 r23: every leaf stamps its palette state (all-zero for an
    // inter leaf or a non-palette intra one, real for a palette block) into
    // the mi-granular neighbour bands, so the next intra block's
    // `av1_get_palette_mode_ctx` and colour cache see exactly what libaom's
    // mode-info grid holds.
    record_strip_palette(
        neighbours,
        leaf_mi,
        8,
        8,
        palette_record8.0,
        palette_record8.1,
        palette_record8.2,
        palette_record8.3,
    );
    let saved_chroma_ctx = chroma_stamped_per_unit.then(|| {
        (
            [
                [neighbours.left[leaf_mi.0][1], neighbours.left[leaf_mi.0][2]],
                [
                    neighbours.left[leaf_mi.0 + 1][1],
                    neighbours.left[leaf_mi.0 + 1][2],
                ],
            ],
            [
                [
                    neighbours.above[leaf_mi.1][1],
                    neighbours.above[leaf_mi.1][2],
                ],
                [
                    neighbours.above[leaf_mi.1 + 1][1],
                    neighbours.above[leaf_mi.1 + 1][2],
                ],
            ],
        )
    });
    neighbours.record_mi(leaf_mi, 8, &[&luma_grid[..], &u_grid[..], &v_grid[..]]);
    if let Some((left, above)) = saved_chroma_ctx {
        // The walk's per-unit stamps are the block's real chroma coefficient
        // contexts (`av1_set_contexts` per TX_4X4 unit); put them back over
        // `record_mi`'s one composed state.
        for (cell, uv) in left.into_iter().enumerate() {
            neighbours.left[leaf_mi.0 + cell][1] = uv[0];
            neighbours.left[leaf_mi.0 + cell][2] = uv[1];
        }
        for (cell, uv) in above.into_iter().enumerate() {
            neighbours.above[leaf_mi.1 + cell][1] = uv[0];
            neighbours.above[leaf_mi.1 + cell][2] = uv[1];
        }
    }
    // lane-uv8 r1: this leaf never stamped a mode band at all, so the block
    // below/right of it read whatever block last wrote the coarse [`SUB`]
    // slot -- now that an intra leaf here can code a smooth/directional
    // `uv_mode`, its chroma neighbours must see it (and an inter leaf must
    // stamp `DC_PRED`, what libaom stores for a non-intra block). Same pair
    // [`decode_leaf8`] writes, plus the coarse fallback slots.
    neighbours.above_uv_mode[c] = uv_predict_mode;
    neighbours.left_uv_mode[r] = uv_predict_mode;
    neighbours.record_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, mode_for_tx);
    neighbours.record_uv_mode_mi(leaf_mi.0, leaf_mi.1, 2, 2, uv_predict_mode);
    if let Some((left, above)) = saved_luma_ctx {
        for (cell, state) in left.into_iter().enumerate() {
            neighbours.left[leaf_mi.0 + cell][0] = state;
        }
        for (cell, state) in above.into_iter().enumerate() {
            neighbours.above[leaf_mi.1 + cell][0] = state;
        }
    }
    // lane-gmaffine r2: the deblock grid's ref id is the leaf's OWN reference
    // (r1 taught this leaf non-LAST refs; the hardcoded LAST_FRAME made every
    // GOLDEN/ALTREF leaf read as LAST at the loop-filter's ref/mv edge test).
    // lane-cdef r1: the fall-through twin of the compound arm's skip-band
    // write above -- `skip` is one flag for the whole 8x8 leaf (read at
    // `decode.rs`'s `skip_ctx` site), `split8` only splits its transform.
    // lane-intersub8 r5: ... and it counts too. r1 incremented the counter in
    // the COMPOUND early-return arm only, so the gate's assert really read
    // "a COMPOUND 8x8 leaf wrote the band"; once main's inter16ab/r14 routing
    // landed, the 10-bit cq12 arm's leaves come through here instead and the
    // gate failed on an instrumentation gap, with the band written either way.
    hit!(INTER8_SKIP_BAND_HITS);
    neighbours.fill_skip_grid(leaf_mi, 2, skip);
    neighbours.fill_lf_grid(
        leaf_mi,
        2,
        if split8 { 4 } else { 8 },
        if is_inter {
            leaf_refs.0.max(LAST_FRAME)
        } else {
            0
        },
        fctx,
    );
    Ok((
        skip,
        is_inter,
        skip_mode,
        compound_ctx8,
        leaf_filter_syms,
        leaf_refs,
    ))
}

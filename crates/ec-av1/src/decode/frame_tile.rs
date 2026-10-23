//! Decoder for one inter-frame tile payload: the tile walk that drives the
//! `decode_inter_block` family over a whole tile, with and without loop-restoration
//! units. Moved out of `decode/mod.rs` (av1split D5); the items are unchanged.

use super::*;

/// Decodes the payload [`crate::tile::sb_coeff_inter_frame_tile`] writes,
/// against `reference` (the previous frame's own decoded picture, at this
/// frame's true size -- a decoder's single-slot DPB, matching what this
/// crate's encoder always predicts from), returning the picture it
/// reconstructs to. Mirrors [`decode_key_frame_tile`]'s own contract
/// (`mi_cols`/`mi_rows`/`base_q_idx` from the frame header,
/// `frame_width`/`frame_height` its true render size).
///
/// # Errors
/// Returns an error under the same conditions [`decode_inter_block`] does,
/// or when `reference` is not this frame's own true size, or when the tile
/// codes a 16x16 leaf whose own true edge cuts through it (round 2, same
/// refusal [`crate::tile`]'s writer gives its encoder).
#[allow(dead_code)]
// read only from the `#[cfg(test)]` gates
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_inter_frame_tile(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    refpix: &RefPix<'_>,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    reference_select: bool,
    // lane-intertx: this frame header's own `reduced_tx_set` (spec 5.9.2) --
    // see [`decode_inter_frame_tile_lr`]'s parameter. Same stale-header
    // class as `tx_select` below.
    reduced_tx_set: bool,
    // This frame header's `tx_mode == TxMode::Select`: `Encoded::tx_select`.
    // Hard-coding `false` here desynced at the first `tx_depth` symbol once
    // inter frames coded Select by default (class: test asserts against a
    // stale header, second instance).
    tx_select: bool,
    // The CDF state this frame's tile was WRITTEN from (`Encoded::start_cdfs`):
    // an inter frame whose header leaves `disable_frame_end_update_cdf` off
    // starts from what the previous frame stored, not from the defaults. Same
    // class as `tx_select` above, third instance.
    initial_cdfs: Option<Cdfs>,
    // lane-av1obmc: this frame header's own `is_motion_mode_switchable`
    // (spec 5.9.2). Hard-coding `false` here desynced every stream this
    // crate's own encoder writes once inter blocks started carrying a
    // `motion_mode` symbol (class: test asserts against a stale header).
    switchable_motion_mode: bool,
    fctx: &crate::decode::FrameCtx,
) -> Result<Picture> {
    decode_inter_frame_tile_lr(
        data,
        mi_cols,
        mi_rows,
        base_q_idx,
        frame_width,
        frame_height,
        refpix,
        cdef,
        loop_filter,
        allow_high_precision_mv,
        force_integer_mv,
        interp_fixed,
        enable_dual_filter,
        reference_select,
        reduced_tx_set,
        tx_select,
        &LoopRestorationParams::default(),
        initial_cdfs,
        NO_SIGN_BIAS,
        false,
        // `enable_filter_intra`: as the `false`s beside it, this convenience
        // entry names a stream written without the sequence bit (lane-fintra).
        switchable_motion_mode,
        false,
        false,
        DeltaParams::default(),
        fctx,
    )
}

/// [`decode_inter_frame_tile`] with this frame header's own `lr_params`, the
/// inter sibling of [`decode_key_frame_tile_lr`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_inter_frame_tile_lr(
    data: &[u8],
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    refpix: &RefPix<'_>,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    reference_select: bool,
    // lane-intertx: this frame header's own `reduced_tx_set` (spec 5.9.2).
    // The wrapper used to hard-code `true` ("the tests all code the reduced
    // sets") -- true until the widened inter search put 1-D/DTT types on
    // `reduced_tx_set = 0` frames' inter luma, whose CHROMA then inherits
    // the type against this very bit
    // ([`reduce_inherited_chroma_tx_type`]): guessing `true` mis-derives
    // every inherited chroma type and desyncs the tile (class: test asserts
    // against a stale header, next instance).
    reduced_tx_set: bool,
    tx_select: bool,
    lr: &LoopRestorationParams,
    initial_cdfs: Option<Cdfs>,
    // spec 5.9.2's `ref_frame_sign_bias`, derived by a real decoder from the
    // order hints its DPB slots hold. A raw tile decode cannot derive it (it
    // is handed planes, not slots), and hard-coding `NO_SIGN_BIAS` here
    // desyncs the encoder's own filter-search re-decode of any frame with a
    // BACKWARD reference -- a pyramid leaf reading its group's hidden
    // `ALTREF_FRAME` -- since the writer's MV-stack scans flip borrowed
    // candidates across the bias boundary and this one's did not. Same class
    // as `tx_select`/`initial_cdfs` above: a header-derived value a raw tile
    // decode has to be told.
    sign_bias: crate::mvstack::SignBiasTable,
    // This frame header's own `allow_screen_content_tools`: its intra blocks
    // then carry the palette syntax (spec 5.11.46), which a raw tile decode
    // that guessed `false` reads one symbol short of. Placed after
    // `sign_bias` rather than beside the other bools on purpose (the
    // `adjacent-same-typed-args` class).
    allow_screen_content_tools: bool,
    // lane-av1obmc: this frame header's own `is_motion_mode_switchable`
    // (spec 5.9.2). Hard-coding `false` here desynced every stream this
    // crate's own encoder writes once inter blocks started carrying a
    // `motion_mode` symbol (class: test asserts against a stale header).
    switchable_motion_mode: bool,
    // lane-av1obmc2: this frame header's own `allow_warped_motion` (spec
    // 5.9.2) -- what picks the 3-symbol `motion_mode_cdf` alphabet over the
    // 2-symbol `obmc_cdf` one at a block with a warp sample. Hard-coding
    // `false` here desynced every stream this crate's encoder writes with the
    // warp knob on: the same stale-header class as the parameter above.
    allow_warped_motion: bool,
    // lane-fintra: see [`decode_inter_frame_tiles_lr`]'s own parameter.
    enable_filter_intra: bool,
    // lane-deltaq: see [`decode_inter_frame_tiles_lr`]'s own parameter.
    delta: DeltaParams,
    fctx: &crate::decode::FrameCtx,
) -> Result<Picture> {
    let single_tile = TileInfo {
        mi_col_starts: vec![0, mi_cols],
        mi_row_starts: vec![0, mi_rows],
        ..TileInfo::default()
    };
    decode_inter_frame_tiles_lr(
        &[data],
        &single_tile,
        mi_cols,
        mi_rows,
        base_q_idx,
        frame_width,
        frame_height,
        refpix,
        cdef,
        loop_filter,
        allow_high_precision_mv,
        force_integer_mv,
        interp_fixed,
        enable_dual_filter,
        reference_select,
        // lane-intertx: this frame's own bit, not the hardcoded `true` --
        // see the parameter's note above.
        reduced_tx_set,
        tx_select,
        lr,
        initial_cdfs,
        sign_bias,
        allow_screen_content_tools,
        switchable_motion_mode,
        allow_warped_motion,
        enable_filter_intra,
        delta,
        fctx,
    )
}

/// [`decode_inter_frame_tile_lr`] for a frame coded as SEVERAL tiles; see
/// [`decode_key_frame_tiles_lr`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_inter_frame_tiles_lr(
    tiles: &[&[u8]],
    tile_info: &TileInfo,
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    frame_width: u32,
    frame_height: u32,
    refpix: &RefPix<'_>,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    reference_select: bool,
    // lane-txi: this frame header's own `reduced_tx_set` bit. It used to be
    // hardcoded `true` at the call below -- the encoder's own trial decode of
    // the tile it just wrote then read every luma `tx_type` out of the
    // reduced alphabet whatever the header said (class: a header bit a raw
    // tile decode cannot guess, the same one `tx_select` carries).
    reduced_tx_set: bool,
    tx_select: bool,
    lr: &LoopRestorationParams,
    initial_cdfs: Option<Cdfs>,
    sign_bias: crate::mvstack::SignBiasTable,
    // This frame header's own `allow_screen_content_tools` (see the
    // single-tile wrapper): the palette syntax its intra blocks carry.
    allow_screen_content_tools: bool,
    // lane-av1obmc: this frame header's own `is_motion_mode_switchable`
    // (spec 5.9.2). Hard-coding `false` here desynced every stream this
    // crate's own encoder writes once inter blocks started carrying a
    // `motion_mode` symbol (class: test asserts against a stale header).
    switchable_motion_mode: bool,
    // lane-av1obmc2: this frame header's own `allow_warped_motion` (spec
    // 5.9.2) -- what picks the 3-symbol `motion_mode_cdf` alphabet over the
    // 2-symbol `obmc_cdf` one at a block with a warp sample. Hard-coding
    // `false` here desynced every stream this crate's encoder writes with the
    // warp knob on: the same stale-header class as the parameter above.
    allow_warped_motion: bool,
    // lane-fintra: this sequence header's own `enable_filter_intra` bit (spec
    // 5.5.2). Hard-coded `false` here until this lane -- the same stale-header
    // class as `switchable_motion_mode`/`allow_warped_motion` above: the
    // encoder's own trial decode desyncs at the first intra-in-inter DC_PRED
    // block the moment its sequence header sets the bit.
    enable_filter_intra: bool,
    // lane-deltaq: this frame header's own `delta_q_params`/`delta_lf_params`
    // (spec 5.9.17/5.9.18) -- the same stale-header class as the three
    // parameters above. Hard-coded `DeltaParams::default()` here until this
    // lane, which desyncs the encoder's own trial decode at the first
    // superblock the moment its header sets `delta_q_present`.
    delta: DeltaParams,
    fctx: &crate::decode::FrameCtx,
) -> Result<Picture> {
    decode_inter_frame_tile_with_cdfs(
        tiles,
        tile_info,
        mi_cols,
        mi_rows,
        base_q_idx,
        crate::quant::QuantDeltas::default(),
        frame_width,
        frame_height,
        refpix,
        cdef,
        loop_filter,
        lr,
        initial_cdfs,
        allow_high_precision_mv,
        force_integer_mv,
        sign_bias,
        [ec_av1_syntax::WarpParams::default(); 7],
        interp_fixed,
        enable_dual_filter,
        0,
        0,
        [0; 7],
        None,
        reference_select,
        false,
        false,
        false,
        false,
        [0; 2],
        reduced_tx_set,
        tx_select,
        switchable_motion_mode,
        allow_warped_motion,
        allow_screen_content_tools,
        delta,
        enable_filter_intra,
        None,
        fctx,
    )
    .map(|(picture, _, _)| picture)
}

/// [`decode_inter_frame_tile`], threading a cross-frame CDF forward -- see
/// [`decode_key_frame_tile_with_cdfs`]'s doc for `initial_cdfs`/the returned
/// end-of-tile table. An inter frame's own header can name
/// `primary_ref_frame == PRIMARY_REF_NONE` too (an error-resilient stream),
/// so `None` is a real case here, not only a convenience default.
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_inter_frame_tile_with_cdfs(
    tiles: &[&[u8]],
    tile_info: &TileInfo,
    mi_cols: u32,
    mi_rows: u32,
    base_q_idx: u8,
    // lane-sbpart r11: see [`decode_key_frame_tile_with_cdfs`]'s own `deltas`.
    deltas: crate::quant::QuantDeltas,
    frame_width: u32,
    frame_height: u32,
    refpix: &RefPix<'_>,
    cdef: &CdefParams,
    loop_filter: &LoopFilterParams,
    lr: &LoopRestorationParams,
    initial_cdfs: Option<Cdfs>,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    sign_bias_table: SignBiasTable,
    // lane-gm r2: this frame header's own `global_motion` table (spec
    // 5.9.24), threaded to every `decode_inter_block` call below.
    global_motion: [ec_av1_syntax::WarpParams; 7],
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    // lane-av1tmvp: `order_hint`/`order_hint_bits`/`ref_order_hints` are
    // always needed (this frame's own saved [`crate::motion_field::MotionField`]
    // carries them for a *later* frame's projection, spec 7.9); `tpl_field`
    // is `Some` only when this frame's own header set `use_ref_frame_mvs`
    // and the caller ([`crate::stream::decode_stream`]) projected one.
    order_hint_bits: u32,
    order_hint: u32,
    ref_order_hints: [u32; 7],
    tpl_field: Option<&crate::motion_field::TplField>,
    // lane-av1comp: this frame header's own `reference_select` bit, threaded
    // to every `decode_inter_block`/`decode_inter_block8` call below.
    reference_select: bool,
    // lane-av1comp: `seq_params`' own compound-blend enable bits.
    enable_masked_compound: bool,
    enable_jnt_comp: bool,
    // lane-sb128 r3: this sequence header's own `enable_interintra_compound`
    // bit, threaded to every `decode_inter_block`/`decode_inter_block8` call
    // below (spec 5.11.24's `interintra` read).
    enable_interintra_compound: bool,
    // lane-av1comp round 14: this frame header's own `skip_mode_present`/
    // `skip_mode_frame` (spec 5.9.22), threaded to every `decode_inter_block`/
    // `decode_inter_block8` call below.
    skip_mode_present: bool,
    skip_mode_frame: [u8; 2],
    // lane-cdffwd2: this frame header's own `reduced_tx_set` bit, threaded to
    // every inter `tx_type` read below (`false` needs the 12-/16-symbol
    // `Set1` coefficient tables at 16x16/8x8 rather than the reduced
    // 2-symbol ones -- see [`TxbSet::Luma16InterSet1`]/[`TxbSet::Luma8InterSet1`]).
    reduced_tx_set: bool,
    // lane-txselect: this frame header's own `tx_mode == TxMode::Select`
    // bit (spec 5.9.14). Together with `reduced_tx_set` it is stashed in a
    // thread-local by [`set_inter_tx_mode`] rather than threaded through
    // `decode_inter_block`'s forty parameters -- both are frame-constant, and
    // only [`read_block_tx_size`]/the var-tx residual loop read them.
    tx_select: bool,
    // lane-motionmode round 1/3: this frame header's own `is_motion_mode_switchable`/
    // `allow_warped_motion` bits, threaded to every `decode_inter_block`/
    // `decode_inter_block8` call below (spec 5.11.24's `read_motion_mode`).
    switchable_motion_mode: bool,
    allow_warped_motion: bool,
    // lane-screen r2: this frame header's own `allow_screen_content_tools`
    // bit, threaded to every `decode_inter_block`/`decode_inter_block8`
    // call below -- consumes (never reconstructs) the intra-sub-block
    // palette syntax libaom's `read_intra_block_mode_info` reads under the
    // same `av1_allow_palette` gate as the key-frame path.
    allow_screen_content_tools: bool,
    // lane-realworld r5: this frame header's own `delta` (spec 5.9.17/5.9.18),
    // same contract as [`decode_key_frame_tile_with_cdfs`]'s own `delta`
    // param -- only `q_present`/`q_res` are read.
    delta: DeltaParams,
    // lane-fiinter r1: this sequence header's own `enable_filter_intra` bit
    // (spec 5.5.2), stashed in a thread-local with `tx_select` rather than
    // threaded through `decode_inter_block`'s fifty parameters -- read by the
    // intra-in-inter arms' `read_filter_intra_mode_info`.
    enable_filter_intra: bool,
    // lane-twostage: called with this frame's own end-of-tile CDFs, motion
    // field and picture size the moment its last tile symbol is read --
    // BEFORE its filters run, so a frame that references this one can start
    // parsing while this one is still being deblocked/CDEF'd/restored (it
    // waits for the pixels themselves at its first prediction). `None`, and
    // the motion field comes back in the return tuple as it always did:
    // the single-threaded path, and a superres frame (whose final width is
    // only known after the upscale).
    stage1: Option<&dyn Fn(Cdfs, crate::motion_field::MotionField, usize, usize)>,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Picture, Cdfs, Option<crate::motion_field::MotionField>)> {
    set_inter_tx_mode(tx_select, reduced_tx_set, enable_filter_intra, fctx);
    let tpl_frame = tpl_field.map(|field| TplFrameArgs {
        field,
        order_hint_bits,
        order_hint,
        ref_order_hints,
    });
    if mi_cols == 0 || mi_rows == 0 {
        return Err(unsupported("a frame with no mode-info grid"));
    }
    let (true_width, true_height) = ((mi_cols * 4) as usize, (mi_rows * 4) as usize);
    // spec 7.11.3.3: a reference may differ in size from this frame on either
    // axis (a scaled reference, `mc::RefScale`), within the conformance
    // limits: at most 2x down and 16x up. Outside them is a named refusal.
    for rf in 1..=7i8 {
        if let Some((rw, rh)) = refpix.dims(rf)
            && mc::RefScale::out_of_limits(rw, rh, frame_width as usize, frame_height as usize)
        {
            return Err(unsupported(
                "a reference picture outside the 2x down / 16x up scaling limits",
            ));
        }
    }
    let (cols, rows) = block_grid(mi_cols, mi_rows);
    let sb128 = sb128(fctx);
    let (sb_cols, sb_rows) = (cols.div_ceil(2), rows.div_ceil(2));
    fctx.cdef_bits.with(|c| c.set(cdef.bits));
    fctx.cdef_sb_cols.with(|c| c.set(sb_cols as usize));
    fctx.cdef_idx_grid
        .with(|g| *g.borrow_mut() = vec![0u8; sb_cols as usize * sb_rows as usize]);
    let (width, height) = (cols as usize * BLOCK, rows as usize * BLOCK);

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
    // spec `decode_tile`: `CurrentQIndex`/`DeltaLF` reset to the frame's own
    // `base_q_idx`/zero at the top of every tile, not once per frame (mirrors
    // `decode_key_frame_tile_with_cdfs`'s per-tile reset below); the
    // delta_q/delta_lf presence/resolution flags themselves are frame-level
    // and set once here.
    fctx.delta_q_present.with(|c| c.set(delta.q_present));
    fctx.delta_q_res.with(|c| c.set(1i32 << delta.q_res));
    fctx.delta_lf_present.with(|c| c.set(delta.lf_present));
    fctx.delta_lf_res.with(|c| c.set(1i32 << delta.lf_res));
    fctx.delta_lf_multi.with(|c| c.set(delta.lf_multi));
    let mut neighbours = Neighbours::new(
        cols as usize * 2,
        rows as usize * 2,
        mi_cols as usize,
        mi_rows as usize,
        fctx,
    );
    let mut grid = MiGrid::new(mi_cols as usize, mi_rows as usize);
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

    let lr_active = (0..3).any(|p| {
        lr.frame_restoration_type[p] != ec_av1_syntax::RestorationType::None
            && lr.loop_restoration_size[p] != 0
    });

    // lane-tiles r6: mirrors decode_key_frame_tile_with_cdfs's own per-tile
    // loop (spec 5.11.2 decode_tile / 7.20 exit_symbol) -- each tile gets
    // its own fresh SymbolDecoder over its own byte range and its own fresh
    // copy of the frame's initial CDFs; only context_update_tile_id's own
    // end-of-tile adapted table becomes the frame's own output.
    for (tile_idx, &data) in tiles.iter().enumerate() {
        let tile_num = tile_idx as u32;
        let (trow, tcol) = (tile_num / tile_info.cols, tile_num % tile_info.cols);
        let mi_row0 = tile_info.mi_row_starts[trow as usize];
        let mi_row1 = tile_info.mi_row_starts[trow as usize + 1];
        let mi_col0 = tile_info.mi_col_starts[tcol as usize];
        let mi_col1 = tile_info.mi_col_starts[tcol as usize + 1];
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
        fctx.current_q_idx.with(|c| c.set(i32::from(base_q_idx)));
        fctx.quant_deltas.with(|c| c.set(deltas));
        fctx.current_delta_lf.with(|c| c.set([0; 4]));
        let mut dec = SymbolDecoder::new(data);
        // lane-av1txr: spec 5.9.2's `disable_cdf_update` -- a frame that sets
        // it codes every symbol against the tables it started with.
        dec.set_cdf_update(!cdf_update_disabled(fctx));
        crate::census::register_cdfs(&cdfs);
        // lane-comppin r9: tile-entry range, the earliest point comparable
        // against aomdec's own `r->ec.rng` right after `aom_reader_init` -- the
        // first ladder rung before any symbol (partition or otherwise) is read.
        if crate::envflags::env_flag!("EC_AV1_TELL") {
            eprintln!(
                "TELL label=tile_init tell={} range={}",
                dec.debug_bitpos(),
                dec.debug_state().0
            );
        }
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
        // lane-tiles r6: an MV candidate scan (mvstack.rs's grid.get) must
        // not reach across a tile boundary any more than intra prediction's
        // PlaneBuf reach does -- bounding the shared grid's own read window
        // per tile is equivalent to threading tile bounds through every
        // find_mv_stack* call site (all four read through this one grid).
        grid.set_tile_bounds(
            mi_row0 as usize,
            mi_col0 as usize,
            mi_row1 as usize,
            mi_col1 as usize,
        );
        hit!(TILE_HITS);

        // lane-sb128 r2: at `sb_size == 128` the tile walks each 128x128
        // superblock's four 64x64 quadrants in libaom `decode_partition` order
        // (TL, TR, BL, BR) and reads the 128 root's own loop-restoration units and
        // partition symbol once per superblock -- exactly what the key-frame tile
        // loop does; the 64-level body below is unchanged, `read_cdef`'s
        // per-64x64-quadrant reset (`CDEF_TRANSMITTED`) already matches libaom's
        // `cdef_idx[(mi_row & 16) >> 4 * 2 + (mi_col & 16) >> 4]` slot per quadrant.
        // lane-sb128c r1: set when the 128 root resolved to `PARTITION_NONE` -- its
        // one 128x128 inter block covers all four 64x64 quadrants, so the remaining
        // three items of this superblock carry no syntax at all (same skip the
        // key-frame tile loop runs).
        let mut sb128_none = false;
        // Superblock-row wavefront: one job per superblock (per 128x128 when
        // `sb128`, since a 128 block writes across its four 64x64 visits).
        // Intrabc frames read the frame being reconstructed, so they stay inline.
        let wave_shift = u32::from(sb128);
        let (wave_r0, wave_c0) = (
            (sb_r0 >> wave_shift) as usize,
            (sb_c0 >> wave_shift) as usize,
        );
        let (wave_rows, wave_cols) = if sb_r1 > sb_r0 && sb_c1 > sb_c0 {
            (
                ((sb_r1 - 1) >> wave_shift) as usize - wave_r0 + 1,
                ((sb_c1 - 1) >> wave_shift) as usize - wave_c0 + 1,
            )
        } else {
            (0, 0)
        };
        // Ends any wavefront this tile leaves running on an error return.
        let _wave_tile = wave::TileScope;
        if !allow_intrabc_frame(fctx) {
            wave::install(
                fctx,
                refpix,
                [&y, &u, &v],
                64 << wave_shift,
                (wave_r0, wave_rows, wave_c0, wave_cols),
            );
        }
        for (sb_r, sb_c, row_start, sb_start) in sb_visit_order(sb_r0, sb_r1, sb_c0, sb_c1, sb128) {
            // The previous superblock's queued writes go to its worker now.
            if sb_start {
                wave::begin_sb(
                    (sb_r >> wave_shift) as usize - wave_r0,
                    (sb_c >> wave_shift) as usize - wave_c0,
                );
            }
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
                let mut sb128_rect = None;
                let mut sb128_ab = None;
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
                        // lane-sb128c r9: an AB shape at the 128 root -- one
                        // 128x64/64x128 half plus two 64x64 quadrants, same visit
                        // order as the key-frame path.
                        sb128_none = true;
                        sb128_ab = Some(part128);
                    }
                    if matches!(part128, PARTITION_HORZ | PARTITION_VERT) {
                        // Two 128x64 (HORZ) or two 64x128 (VERT) inter blocks; the
                        // whole 128x128 root is consumed here, so the remaining
                        // three 64-quadrant visits of this superblock carry no
                        // syntax (same skip the NONE arm runs).
                        sb128_none = true;
                        sb128_rect = Some(part128);
                        fctx.cdef_transmitted.with(|c| c.set(false));
                    }
                    if part128 == PARTITION_NONE {
                        // One 128x128 inter block. Its luma transform is
                        // `max_txsize_rect_lookup[BLOCK_128X128]` = TX_64X64 (four
                        // units, read as var-tx leaves), its uv plane block is
                        // 64x64 with TX_32X32 units (`av1_get_max_uv_txsize`), and
                        // `size_group_lookup[BLOCK_128X128]` is 3, same as 64x64.
                        sb128_none = true;
                        fctx.cdef_transmitted.with(|c| c.set(false));
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
                let sb_at = (sb_r as usize * 4, sb_c as usize * 4);
                // lane-inter4 r1: every rect inter piece below is the same
                // `decode_inter_block` call with only position / true footprint /
                // transform-set differing; the macro keeps the new arms readable
                // instead of six more 60-line copies of the argument list.
                macro_rules! inter_piece {
                    ($at_mi:expr, $side:expr, $li:expr, $lt:expr, $ch:expr, $ltx:expr,
                 $ctx:expr, $sl:expr, $sc:expr, $sg:expr, $ww:expr, $wh:expr) => {
                        decode_inter_block(
                            &mut dec,
                            &mut cdfs,
                            &mut neighbours,
                            &mut grid,
                            $at_mi,
                            $side,
                            mi_cols,
                            mi_rows,
                            &mut y,
                            &mut u,
                            &mut v,
                            refpix,
                            &sign_bias_table,
                            &global_motion,
                            base_q_idx,
                            $li,
                            $lt,
                            $ch,
                            $ltx,
                            $ctx,
                            $sl,
                            $sc,
                            $sg,
                            allow_high_precision_mv,
                            force_integer_mv,
                            interp_fixed,
                            enable_dual_filter,
                            tpl_frame.as_ref(),
                            reference_select,
                            enable_masked_compound,
                            enable_interintra_compound,
                            enable_jnt_comp,
                            order_hint_bits,
                            order_hint,
                            ref_order_hints,
                            skip_mode_present,
                            skip_mode_frame,
                            switchable_motion_mode,
                            allow_warped_motion,
                            // lane-sb128c r3: `reject_residual` is about a
                            // RECTANGULAR strip decoded with square dimensions --
                            // the 128x128 NONE root is square, and a hardcoded
                            // `true` refused every one of them at its first
                            // non-skip block.
                            $ww != $side || $wh != $side,
                            $ww,
                            $wh,
                            allow_screen_content_tools,
                            (frame_width as usize, frame_height as usize),
                            fctx,
                        )?
                    };
                }
                // lane-inter16ab r1: one 8x8 inter leaf of a 16x16-level AB
                // partition (libaom `decode_partition` PARTITION_HORZ_A/_B/
                // VERT_A/_B at BLOCK_16X16 code `bsize2` = BLOCK_8X8 for two of
                // their three pieces, decodeframe.c ~1900). Same body the
                // PARTITION_SPLIT arm runs per leaf, minus its own partition
                // symbol (an AB piece carries none) and its `prev_leaves`
                // bookkeeping.
                macro_rules! inter_leaf8 {
                ($outer:expr, $leaf:expr) => {{
                    let leaf_mi = $leaf;
                                    let (
                                        skip,
                                        is_inter,
                                        skip_mode_leaf,
                                        compound_ctx8,
                                        leaf_filter,
                                        leaf_refs,
                                    ) = decode_inter_block8(
                                            &mut dec,
                                            &mut cdfs,
                                            &mut neighbours,
                                            &mut grid,
                                            mi_cols,
                                            mi_rows,
                                            $outer,
                                            leaf_mi,
                                            &mut y,
                                            &mut u,
                                            &mut v,
                                            refpix,
                                            base_q_idx,
                                            &scan8,
                                            &scan4,
                                            allow_high_precision_mv,
                                            force_integer_mv,
                                            &global_motion,
                                            enable_dual_filter,
                                            reference_select,
                                            enable_masked_compound,
                                            enable_interintra_compound,
                                            enable_jnt_comp,
                                            order_hint_bits,
                                            order_hint,
                                            ref_order_hints,
                                            skip_mode_present,
                                            skip_mode_frame,
                                            reduced_tx_set,
                                            interp_fixed,
                                            switchable_motion_mode,
                                            allow_warped_motion,
                                            allow_screen_content_tools,
                                            &sign_bias_table,
                                            tpl_frame.as_ref(),
                                            (frame_width as usize, frame_height as usize), fctx,
                                        )?;
                                    neighbours.record_inter_rect_mi(
                                        leaf_mi,
                                        2,
                                        2,
                                        skip,
                                        is_inter,
                                        leaf_refs.0,
                                        leaf_filter,
                                        skip_mode_leaf,
                                    );
                                    if let Some(ref1) = leaf_refs.1
                                        && let Some((_, _, group_idx, idx)) = compound_ctx8
                                    {
                                        neighbours.record_compound_ctx_rect_mi(
                                            leaf_mi, 2, 2, ref1, group_idx, idx,
                                        );
                                    }
                }};
            }
                if let Some(part128) = sb128_ab {
                    // libaom `decode_partition` (decodeframe.c): HORZ_A = TL, TR,
                    // bottom half; HORZ_B = top half, BL, BR; VERT_A = TL, BL,
                    // right half; VERT_B = left half, TR, BR. AB only comes from
                    // the full alphabet symbol (both halves in frame), so there is
                    // no `i > 0` edge break to mirror.
                    let at128 = ((sb_r & !1) as usize * 4, (sb_c & !1) as usize * 4);
                    let base_mi = sub16_to_mi(at128);
                    let (br_mi, bc_mi) = (base_mi.0 + 16, base_mi.1 + 16);
                    let (q_r, q_c) = ((sb_r & !1) as usize, (sb_c & !1) as usize);
                    let _vert_ab = (part128 == PARTITION_VERT_A || part128 == PARTITION_VERT_B)
                        .then(crate::encode::Reach::vert_ab_partition);
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
                    macro_rules! square64_inter {
                        ($at_mi:expr) => {{
                            fctx.cdef_transmitted.with(|c| c.set(false));
                            inter_piece!(
                                $at_mi,
                                SB,
                                TxbSet::Luma64,
                                TxbSet::Luma64,
                                TxbSet::Chroma32,
                                TX32,
                                TX32,
                                &scan32,
                                &scan32,
                                3,
                                SB,
                                SB
                            );
                        }};
                    }
                    macro_rules! half128_inter {
                        ($at_mi:expr, $bw:expr, $bh:expr, $src:expr, $dst:expr) => {{
                            fctx.cdef_transmitted.with(|c| c.set(false));
                            inter_piece!(
                                $at_mi,
                                SB * 2,
                                TxbSet::Luma64,
                                TxbSet::Luma64,
                                TxbSet::Chroma32,
                                TX32,
                                TX32,
                                &scan32,
                                &scan32,
                                3,
                                $bw,
                                $bh
                            );
                            cdef_copy($src, $dst);
                        }};
                    }
                    // lane-av1unwritten r5: the partition symbol is RESOLVED (a full CDF read or
                    // an edge `gather`) and about to be dispatched. `partition_subsize_dims` is
                    // libaom's `get_partition_subsize` (common_data.h:71-90) reduced to
                    // dimensions, so this is `decode_partition`'s pre-descend check
                    // (decodeframe.c:1449-1458) applied to EVERY arm at once -- VERT,
                    // VERT_A / VERT_B, HORZ_4 and VERT_4 included. The HORZ siblings keep
                    // decoding because the helper is exact, not a heuristic. (inter-frame duplicate)
                    refuse_invalid_subsize((128, 128), part128, fctx)?;
                    match part128 {
                        PARTITION_HORZ_A => {
                            square64_inter!(base_mi);
                            square64_inter!((base_mi.0, bc_mi));
                            half128_inter!((br_mi, base_mi.1), 128, 64, (1, 0), (1, 1));
                        }
                        PARTITION_HORZ_B => {
                            half128_inter!(base_mi, 128, 64, (0, 0), (0, 1));
                            square64_inter!((br_mi, base_mi.1));
                            square64_inter!((br_mi, bc_mi));
                        }
                        PARTITION_VERT_A => {
                            square64_inter!(base_mi);
                            square64_inter!((br_mi, base_mi.1));
                            half128_inter!((base_mi.0, bc_mi), 64, 128, (0, 1), (1, 1));
                        }
                        _ => {
                            half128_inter!(base_mi, 64, 128, (0, 0), (1, 0));
                            square64_inter!((base_mi.0, bc_mi));
                            square64_inter!((br_mi, bc_mi));
                        }
                    }
                    continue;
                }
                if let Some(part128) = sb128_rect {
                    // lane-sb128c r7: libaom `decode_partition` (decodeframe.c:1554)
                    // codes PARTITION_HORZ at the 128 root as `subsize =
                    // BLOCK_128X64` at (mi_row, mi_col) and a second one at
                    // (mi_row + hbs, mi_col) only `if (mi_row + hbs < mi_rows)`;
                    // PARTITION_VERT mirrors it on the column. `hbs` here is
                    // `mi_size_high[BLOCK_128X128] / 2` = 16 mi.
                    let horz = part128 == PARTITION_HORZ;
                    let at128 = ((sb_r & !1) as usize * 4, (sb_c & !1) as usize * 4);
                    let base_mi = sub16_to_mi(at128);
                    let (bw, bh) = if horz { (128usize, 64usize) } else { (64, 128) };
                    for i in 0..2 {
                        let at_mi = if horz {
                            (base_mi.0 + i * 16, base_mi.1)
                        } else {
                            (base_mi.0, base_mi.1 + i * 16)
                        };
                        if i > 0 && (at_mi.0 >= mi_rows as usize || at_mi.1 >= mi_cols as usize) {
                            break;
                        }
                        // Each half reads its OWN `cdef_idx` literal: libaom keeps
                        // `cdef_transmitted[4]`, one flag per 64x64 CDEF unit of a
                        // 128x128 superblock (`read_cdef`, decodemv.c:50-81), and
                        // the two halves of a 128-root HORZ/VERT sit in different
                        // units (index `col_in_sb != 0` + 2 * `row_in_sb != 0`).
                        // The key-frame 128 loop already resets here; without it
                        // the second half skipped a `cdef_bits`-wide literal that
                        // libaom reads, desyncing the tile at that block.
                        fctx.cdef_transmitted.with(|c| c.set(false));
                        inter_piece!(
                            at_mi,
                            SB * 2,
                            TxbSet::Luma64,
                            TxbSet::Luma64,
                            TxbSet::Chroma32,
                            TX32,
                            TX32,
                            &scan32,
                            &scan32,
                            3,
                            bw,
                            bh
                        );
                        if horz {
                            hit!(INTER_SB128_HORZ_HITS);
                        } else {
                            hit!(INTER_SB128_VERT_HITS);
                        }
                    }
                    // One `cdef_idx` per BLOCK, copied to every 64x64 CDEF unit it
                    // covers -- both halves of the root read their own, and the
                    // first one's read lands in the top-left unit while the
                    // second's lands in the bottom-left (HORZ) / top-right (VERT).
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
                if sb128_none {
                    hit!(INTER_SB128_NONE_HITS);
                    let at128 = ((sb_r & !1) as usize * 4, (sb_c & !1) as usize * 4);
                    // lane-sb128c r3: `at128` is in SUB16 units like `sb_at`;
                    // `decode_inter_block` takes MI. Unconverted it was (0,0) for
                    // the first 128 root and a quarter-position for every other
                    // one -- the whole-frame gate never got past superblock 1.
                    inter_piece!(
                        sub16_to_mi(at128),
                        SB * 2,
                        TxbSet::Luma64,
                        TxbSet::Luma64,
                        TxbSet::Chroma32,
                        TX32,
                        TX32,
                        &scan32,
                        &scan32,
                        3,
                        SB * 2,
                        SB * 2
                    );
                    // libaom writes one `cdef_strength` into the block's own
                    // `MB_MODE_INFO`, which all four 64x64 CDEF units point at
                    // (mirrors the key-frame 128 NONE block).
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
                let sb_ctx = neighbours.partition_ctx(sb_at, SB);
                let (has_cols, has_rows) = (
                    sb_c * SB_MI + SB_MI / 2 < mi_cols,
                    sb_r * SB_MI + SB_MI / 2 < mi_rows,
                );
                // The straddle cases (one or both halves outside the true
                // frame) never carry a real alphabet symbol -- spec forces
                // SPLIT there (mirrors the intra key-frame tile's own
                // three-way write above). Only the (true, true) case can name
                // a non-SPLIT value, and this loop below only ever recurses as
                // SPLIT -- so a real non-SPLIT part64 here must refuse by name
                // instead of silently decoding as SPLIT and desyncing.
                let part64 = match (has_cols, has_rows) {
                    (true, true) => {
                        let pre_rng = dec.debug_state().0;
                        if crate::envflags::env_flag!("EC_TRACE_PART") {
                            eprintln!(
                                "EC_PART mi_row={} mi_col={} bsize=12 ctx={} rng={}",
                                sb_r as usize * SB_MI as usize,
                                sb_c as usize * SB_MI as usize,
                                sb_ctx,
                                dec.debug_state().0
                            );
                        }
                        let p = dec.symbol(&mut cdfs.partition_w64[sb_ctx]);
                        // lane-av1unwritten r6: the partition symbol is RESOLVED and about to be
                        // dispatched. `partition_subsize_dims` is libaom's `get_partition_subsize`
                        // (common_data.h:71-90) reduced to dimensions, so this is
                        // `decode_partition`'s pre-descend check (decodeframe.c:1451-1460) on EVERY
                        // arm -- VERT, VERT_A / VERT_B, HORZ_4 and VERT_4 -- and the HORZ siblings
                        // keep decoding because the helper is exact, not a heuristic. inter-frame 64x64 level; `p` is the symbol.

                        if crate::envflags::env_flag!("EC_AV1_TRACE") {
                            eprintln!(
                                "TRACE partition_w64 mi=({},{}) ctx={sb_ctx} value={p} pre_rng={pre_rng}",
                                sb_r * SB_MI,
                                sb_c * SB_MI
                            );
                        }
                        p
                    }
                    (true, false) => {
                        // lane-golomb r1 (class `parsed-then-discarded`): the gathered
                        // bit IS the partition -- 0 means PARTITION_HORZ, only 1 means SPLIT
                        // (libaom `ec_read_partition_impl`).
                        if crate::envflags::env_flag!("EC_TRACE_PART") {
                            eprintln!(
                                "EC_PARTG mi_row={} mi_col={} bsize=12 ctx={} rng={}",
                                sb_r as usize * SB_MI as usize,
                                sb_c as usize * SB_MI as usize,
                                sb_ctx,
                                dec.debug_state().0
                            );
                        }
                        if bump_edge32_bit(
                            4,
                            dec.symbol_fixed(&gather(&cdfs.partition_w64[sb_ctx], VERT_ALIKE)),
                        ) == 1
                        {
                            PARTITION_SPLIT
                        } else {
                            PARTITION_HORZ
                        }
                    }
                    (false, true) => {
                        // lane-golomb r1 (class `parsed-then-discarded`): the gathered
                        // bit IS the partition -- 0 means PARTITION_VERT, only 1 means SPLIT
                        // (libaom `ec_read_partition_impl`).
                        if bump_edge32_bit(
                            4,
                            dec.symbol_fixed(&gather(&cdfs.partition_w64[sb_ctx], HORZ_ALIKE)),
                        ) == 1
                        {
                            PARTITION_SPLIT
                        } else {
                            PARTITION_VERT
                        }
                    }
                    (false, false) => {
                        bump_edge_both_cut(2);
                        PARTITION_SPLIT
                    }
                };
                // lane-av1unwritten r7: the symbol is RESOLVED (a full CDF
                // read or an edge `gather`) and about to be dispatched, so the
                // check sits HERE rather than inside the (true, true) arm: the
                // edge arms return without passing it, and (false, true)
                // resolves to PARTITION_VERT, which on a 64x64 root selects
                // BLOCK_32X64 -- INVALID at (1,0) and unreachable from the
                // corpus (measured: 50 reaches corpus-wide, 0 in any 4:2:2 cell).
                // r6 had this inside the (true, true) arm, where it could not
                // see it.
                refuse_invalid_subsize((64, 64), part64, fctx)?;
                if part64 == PARTITION_NONE {
                    // lane-inter8 r1: the whole superblock as one 64x64 inter
                    // block. `SB`-sized syntax throughout (size_group 3, same as
                    // 32x32 -- `size_group_lookup[BLOCK_64X64] == 3`), luma
                    // coefficients through `TxbSet::Luma64` with the 32-point
                    // scan (TX_64X64 codes only its top-left corner, spec
                    // 5.11.40 -- `read_inter_plane` widens the corner back to
                    // 64x64 before the inverse transform), chroma as a plain
                    // 32x32 `TxbSet::Chroma32`. TX_64X64 carries no `tx_type`
                    // symbol at all, so the intra/inter luma sets coincide.
                    hit!(INTER_SB_NONE_HITS);
                    decode_inter_block(
                        &mut dec,
                        &mut cdfs,
                        &mut neighbours,
                        &mut grid,
                        sub16_to_mi(sb_at),
                        SB,
                        mi_cols,
                        mi_rows,
                        &mut y,
                        &mut u,
                        &mut v,
                        refpix,
                        &sign_bias_table,
                        &global_motion,
                        base_q_idx,
                        TxbSet::Luma64,
                        TxbSet::Luma64,
                        TxbSet::Chroma32,
                        TX32,
                        TX32,
                        scan32,
                        scan32,
                        3,
                        allow_high_precision_mv,
                        force_integer_mv,
                        interp_fixed,
                        enable_dual_filter,
                        tpl_frame.as_ref(),
                        reference_select,
                        enable_masked_compound,
                        enable_interintra_compound,
                        enable_jnt_comp,
                        order_hint_bits,
                        order_hint,
                        ref_order_hints,
                        skip_mode_present,
                        skip_mode_frame,
                        switchable_motion_mode,
                        allow_warped_motion,
                        false,
                        SB,
                        SB,
                        allow_screen_content_tools,
                        (frame_width as usize, frame_height as usize),
                        fctx,
                    )?;
                    continue;
                }
                if matches!(
                    part64,
                    PARTITION_HORZ | PARTITION_VERT | PARTITION_HORZ_4 | PARTITION_VERT_4
                ) {
                    // lane-av1unwritten r6: arm census.
                    note_subsize_arm(
                        part64,
                        chroma_plane_block_codable(
                            partition_subsize_dims((64, 64), part64).0,
                            partition_subsize_dims((64, 64), part64).1,
                            ss_x(fctx),
                            ss_y(fctx),
                        ),
                    );
                    // lane-inter4 r1: the superblock-level rect + 1:4 pairs as
                    // inter blocks -- two 64x32 / 32x64 strips, or four 64x16 /
                    // 16x64 ones. Each piece keeps `side = SB` for every
                    // syntax/CDF/prediction-buffer decision (the square-context
                    // corner-cut PARTITION_HORZ already ships at the 32 level)
                    // but names its own true footprint through `write_w`/
                    // `write_h`, which is what `motion_mode`'s bsize row, the
                    // mv-stack's bw4/bh4, the delta-q `bSize == sbSize` test and
                    // every neighbour stamp read. `reject_residual = true`: a
                    // non-skip strip refuses by name rather than over-read a
                    // square residual (rectangular inter residual coding is not
                    // ported -- see lanes/inter4-r1.report.md).
                    let horz = matches!(part64, PARTITION_HORZ | PARTITION_HORZ_4);
                    let four = matches!(part64, PARTITION_HORZ_4 | PARTITION_VERT_4);
                    let base_mi = (
                        sb_r as usize * SB_MI as usize,
                        sb_c as usize * SB_MI as usize,
                    );
                    let (n_pieces, step_mi) = if four {
                        (4usize, 4usize)
                    } else {
                        (2usize, 8usize)
                    };
                    let (bw, bh) = match (horz, four) {
                        (true, false) => (64usize, 32usize),
                        (false, false) => (32, 64),
                        (true, true) => (64, 16),
                        (false, true) => (16, 64),
                    };
                    for i in 0..n_pieces {
                        let at_mi = if horz {
                            (base_mi.0 + i * step_mi, base_mi.1)
                        } else {
                            (base_mi.0, base_mi.1 + i * step_mi)
                        };
                        // libaom `decode_partition`'s own `i > 0` frame-edge break.
                        if i > 0 && (at_mi.0 >= mi_rows as usize || at_mi.1 >= mi_cols as usize) {
                            break;
                        }
                        inter_piece!(
                            at_mi,
                            SB,
                            TxbSet::Luma64,
                            TxbSet::Luma64,
                            TxbSet::Chroma32,
                            TX32,
                            TX32,
                            &scan32,
                            &scan32,
                            3,
                            bw,
                            bh
                        );
                        if !four && i == 0 && ((horz && !has_rows) || (!horz && !has_cols)) {
                            bump_inter_edge_strip(if horz { 0 } else { 1 });
                        }
                        match (horz, four) {
                            (true, false) => hit!(INTER_SB_HORZ_HITS),
                            (false, false) => hit!(INTER_SB_VERT_HITS),
                            (true, true) => hit!(INTER_SB_HORZ4_HITS),
                            (false, true) => hit!(INTER_SB_VERT4_HITS),
                        }
                    }
                    continue;
                }
                if (PARTITION_HORZ_A..=PARTITION_VERT_B).contains(&part64) {
                    // lane-sbab r1: the four superblock-level AB arms on the
                    // INTER path -- two 32x32 inter squares plus one true
                    // 64x32/32x64 inter strip, in libaom `decode_partition`'s own
                    // order (decodeframe.c: HORZ_A = TL, TR, bottom strip;
                    // HORZ_B = top strip, BL, BR; VERT_A = TL, BL, right strip;
                    // VERT_B = left strip, TR, BR). Every piece is one this
                    // decoder already proves elsewhere: the 32x32 square is the
                    // `PARTITION_NONE`-under-SPLIT block below, the strip is the
                    // `PARTITION_HORZ`/`VERT` piece above (`inter_piece!`, which
                    // keeps `side = SB` for syntax/CDF and names its true
                    // footprint through `write_w`/`write_h`). The intra-frame
                    // twin of this arm is lane-part32 r4's `SB_AB_HITS` block.
                    hit_do! {
                        SB_AB_INTER_HITS.with(|c| {
                            let mut h = c.get();
                            h[part64 - PARTITION_HORZ_A] += 1;
                            c.set(h);
                        });
                    }
                    // lane-part32 r5 / class visit-order-changes-availability:
                    // only the two VERTICAL arms visit TL, BL, TR, BR, so only
                    // they switch libaom's `has_tr`/`has_bl` tables.
                    let _vert_guard = (part64 == PARTITION_VERT_A || part64 == PARTITION_VERT_B)
                        .then(crate::encode::Reach::vert_ab_partition);
                    let (mi_row0, mi_col0) = (
                        sb_r as usize * SB_MI as usize,
                        sb_c as usize * SB_MI as usize,
                    );
                    // 32 px = 8 MI units.
                    let tl = (mi_row0, mi_col0);
                    let tr = (mi_row0, mi_col0 + 8);
                    let bl = (mi_row0 + 8, mi_col0);
                    let br = (mi_row0 + 8, mi_col0 + 8);
                    macro_rules! ab_strip64 {
                        ($mi:expr, $ww:expr, $wh:expr) => {
                            inter_piece!(
                                $mi,
                                SB,
                                TxbSet::Luma64,
                                TxbSet::Luma64,
                                TxbSet::Chroma32,
                                TX32,
                                TX32,
                                &scan32,
                                &scan32,
                                3,
                                $ww,
                                $wh
                            )
                        };
                    }
                    // The 32x32 squares carry a full square residual, so unlike
                    // `inter_piece!` (which hardcodes `reject_residual = true`
                    // for its strips) they pass `false`.
                    macro_rules! ab_square32 {
                        ($mi:expr) => {
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                $mi,
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                &scan32,
                                &scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                BLOCK,
                                BLOCK,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?
                        };
                    }
                    match part64 {
                        PARTITION_HORZ_A => {
                            ab_square32!(tl);
                            ab_square32!(tr);
                            ab_strip64!(bl, 64, 32);
                        }
                        PARTITION_HORZ_B => {
                            ab_strip64!(tl, 64, 32);
                            ab_square32!(bl);
                            ab_square32!(br);
                        }
                        PARTITION_VERT_A => {
                            ab_square32!(tl);
                            ab_square32!(bl);
                            ab_strip64!(tr, 32, 64);
                        }
                        _ => {
                            ab_strip64!(tl, 32, 64);
                            ab_square32!(tr);
                            ab_square32!(br);
                        }
                    }
                    continue;
                }
                // Every value of the 10-symbol `partition_w64` alphabet is now
                // handled above except SPLIT, which falls through to the quadrant
                // loop below (lane-sbab r1 took the AB four; the rect/1:4 arms
                // and NONE `continue` earlier).
                debug_assert_eq!(part64, PARTITION_SPLIT);

                for quadrant in 0..4 {
                    let (r32, c32) = (sb_r * 2 + quadrant / 2, sb_c * 2 + quadrant % 2);
                    if r32 >= rows || c32 >= cols {
                        continue;
                    }
                    let at = (r32 as usize * 2, c32 as usize * 2);
                    let ctx32 = neighbours.partition_ctx(at, BLOCK);
                    let (has_cols32, has_rows32) = (
                        has_half(c32 * BLOCK_MI, BLOCK_MI, mi_cols),
                        has_half(r32 * BLOCK_MI, BLOCK_MI, mi_rows),
                    );
                    let part32 = if has_cols32 && has_rows32 {
                        if crate::envflags::env_flag!("EC_AV1_TRACE") {
                            eprintln!(
                                "TRACE part32_pre mi=({},{}) rng={}",
                                r32 * BLOCK_MI,
                                c32 * BLOCK_MI,
                                dec.debug_state().0
                            );
                        }
                        // lane-av1tilerows: this rung printed the RAW 32-grid
                        // index (`r32`, `c32`) into its `mi_row=`/`mi_col=`
                        // fields — a factor of `BLOCK_MI` (8) off — while the
                        // `TRACE part32_pre` line immediately above and the
                        // `EC_AV1_TRACE` line immediately below both print
                        // `r32 * BLOCK_MI` correctly. A rung whose coordinates
                        // are wrong is worse than no rung: it cannot be paired
                        // against libaom's per-level `EC_PART_VAL` dump, which
                        // is the only way to locate a partition-walk
                        // divergence. Behaviour-free: env-gated print only.
                        if crate::envflags::env_flag!("EC_TRACE_PART") {
                            eprintln!(
                                "EC_PART mi_row={} mi_col={} bsize=9 ctx={} rng={}",
                                r32 * BLOCK_MI,
                                c32 * BLOCK_MI,
                                ctx32,
                                dec.debug_state().0
                            );
                        }
                        let p = dec.symbol(&mut cdfs.partition_w32[ctx32]);
                        if crate::envflags::env_flag!("EC_AV1_TRACE") {
                            eprintln!(
                                "TRACE partition_w32 mi=({},{}) ctx={ctx32} value={p}",
                                r32 * BLOCK_MI,
                                c32 * BLOCK_MI
                            );
                        }
                        p
                    } else {
                        match (has_cols32, has_rows32) {
                            (true, false) => {
                                // lane-golomb r1 (class `parsed-then-discarded`): the gathered
                                // bit IS the partition -- 0 means PARTITION_HORZ, only 1 means SPLIT
                                // (libaom `ec_read_partition_impl`).
                                if crate::envflags::env_flag!("EC_TRACE_PART") {
                                    eprintln!(
                                        "EC_PARTG mi_row={} mi_col={} bsize=9 ctx={} rng={}",
                                        r32 as usize * BLOCK_MI as usize,
                                        c32 as usize * BLOCK_MI as usize,
                                        ctx32,
                                        dec.debug_state().0
                                    );
                                }
                                if {
                                    let b = dec.symbol_fixed(&gather(
                                        &cdfs.partition_w32[ctx32],
                                        VERT_ALIKE,
                                    ));
                                    bump_edge32(b.min(1));
                                    b
                                } == 1
                                {
                                    PARTITION_SPLIT
                                } else {
                                    PARTITION_HORZ
                                }
                            }
                            (false, true) => {
                                // lane-golomb r1 (class `parsed-then-discarded`): the gathered
                                // bit IS the partition -- 0 means PARTITION_VERT, only 1 means SPLIT
                                // (libaom `ec_read_partition_impl`).
                                if {
                                    let b = dec.symbol_fixed(&gather(
                                        &cdfs.partition_w32[ctx32],
                                        HORZ_ALIKE,
                                    ));
                                    bump_edge32(b.min(1));
                                    b
                                } == 1
                                {
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
                    // lane-av1unwritten r5: the partition symbol is RESOLVED (a full CDF read or
                    // an edge `gather`) and about to be dispatched. `partition_subsize_dims` is
                    // libaom's `get_partition_subsize` (common_data.h:71-90) reduced to
                    // dimensions, so this is `decode_partition`'s pre-descend check
                    // (decodeframe.c:1449-1458) applied to EVERY arm at once -- VERT,
                    // VERT_A / VERT_B, HORZ_4 and VERT_4 included. The HORZ siblings keep
                    // decoding because the helper is exact, not a heuristic. (inter-frame duplicate)
                    refuse_invalid_subsize((32, 32), part32, fctx)?;
                    match part32 {
                        PARTITION_NONE => {
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi(at),
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                scan32,
                                scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                BLOCK,
                                BLOCK,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                        }
                        PARTITION_SPLIT => {
                            for sub in 0..4 {
                                let (sr, sc) =
                                    (r32 as usize * 2 + sub / 2, c32 as usize * 2 + sub % 2);
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
                                // lane-inter8 r1: a straddling 16x16 cannot name a
                                // value (spec forces SPLIT, one gathered bit); an
                                // interior one reads the real alphabet, and
                                // `PARTITION_SPLIT` now recurses into four 8x8
                                // inter leaves through the very same loop the
                                // straddling case has always used.
                                let part16 = if has_cols16 && has_rows16 {
                                    if crate::envflags::env_flag!("EC_TRACE_PART") {
                                        eprintln!(
                                            "EC_PART mi_row={} mi_col={} bsize=6 ctx={} rng={}",
                                            (sr * SUB_MI as usize),
                                            (sc * SUB_MI as usize),
                                            ctx16,
                                            dec.debug_state().0
                                        );
                                    }
                                    let p = dec.symbol(&mut cdfs.partition_w16[ctx16]);
                                    if crate::envflags::env_flag!("EC_AV1_TRACE") {
                                        let (rng, _) = dec.debug_state();
                                        eprintln!(
                                            "TRACE partition_w16 mi=({},{}) ctx={ctx16} value={p} rng={rng}",
                                            sr as u32 * SUB_MI,
                                            sc as u32 * SUB_MI
                                        );
                                    }
                                    p
                                } else if !has_cols16 && !has_rows16 {
                                    // lane-edgeboth r1: both axes cut by the true
                                    // frame edge -- libaom `decode_partition`
                                    // forces PARTITION_SPLIT and reads NO symbol;
                                    // the leaf loop below already drops the
                                    // out-of-frame quadrants.
                                    bump_edge_both_cut(0);
                                    PARTITION_SPLIT
                                } else {
                                    if has_cols16 {
                                        // lane-golomb r1 (class `parsed-then-discarded`): the gathered
                                        // bit IS the partition -- 0 means PARTITION_HORZ, only 1 means SPLIT
                                        // (libaom `ec_read_partition_impl`).
                                        if crate::envflags::env_flag!("EC_TRACE_PART") {
                                            eprintln!(
                                                "EC_PARTG mi_row={} mi_col={} bsize=6 ctx={} rng={}",
                                                sr * SUB_MI as usize,
                                                sc * SUB_MI as usize,
                                                ctx16,
                                                dec.debug_state().0
                                            );
                                        }
                                        if dec.symbol_fixed(&gather(
                                            &cdfs.partition_w16[ctx16],
                                            VERT_ALIKE,
                                        )) == 1
                                        {
                                            PARTITION_SPLIT
                                        } else {
                                            PARTITION_HORZ
                                        }
                                    } else {
                                        // lane-golomb r1 (class `parsed-then-discarded`): the gathered
                                        // bit IS the partition -- 0 means PARTITION_VERT, only 1 means SPLIT
                                        // (libaom `ec_read_partition_impl`).
                                        if dec.symbol_fixed(&gather(
                                            &cdfs.partition_w16[ctx16],
                                            HORZ_ALIKE,
                                        )) == 1
                                        {
                                            PARTITION_SPLIT
                                        } else {
                                            PARTITION_VERT
                                        }
                                    }
                                };
                                // lane-av1unwritten r7: the inter-frame 16x16
                                // level had NO guard at all -- r6 dropped r5's
                                // misplaced one without replacing it, and all three
                                // dispatch arms below (NONE/HORZ/VERT, the four AB
                                // arms, and the 1:4 arms) select BLOCK_8X16 and
                                // BLOCK_4X16, two of the eight (1,0)-INVALID shapes.
                                // The check goes where the symbol is RESOLVED, so
                                // every arm is covered by construction.
                                refuse_invalid_subsize((16, 16), part16, fctx)?;
                                if part16 == PARTITION_NONE {
                                    decode_inter_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        &mut grid,
                                        sub16_to_mi(at16),
                                        SUB,
                                        mi_cols,
                                        mi_rows,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        refpix,
                                        &sign_bias_table,
                                        &global_motion,
                                        base_q_idx,
                                        TxbSet::Luma16,
                                        if reduced_tx_set {
                                            TxbSet::Luma16Inter
                                        } else {
                                            TxbSet::Luma16InterSet1
                                        },
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        scan16,
                                        scan8,
                                        2,
                                        allow_high_precision_mv,
                                        force_integer_mv,
                                        interp_fixed,
                                        enable_dual_filter,
                                        tpl_frame.as_ref(),
                                        reference_select,
                                        enable_masked_compound,
                                        enable_interintra_compound,
                                        enable_jnt_comp,
                                        order_hint_bits,
                                        order_hint,
                                        ref_order_hints,
                                        skip_mode_present,
                                        skip_mode_frame,
                                        switchable_motion_mode,
                                        allow_warped_motion,
                                        false,
                                        SUB,
                                        SUB,
                                        allow_screen_content_tools,
                                        (frame_width as usize, frame_height as usize),
                                        fctx,
                                    )?;
                                } else if part16 == PARTITION_HORZ || part16 == PARTITION_VERT {
                                    // lane-inter4 r3: the 16x16-level rect inter
                                    // leaves -- two true 16x8 (HORZ) or 8x16
                                    // (VERT) inter blocks. Same shape as the
                                    // 32-level HORZ/VERT arm above: `side = SUB`
                                    // keeps every CDF/prediction-buffer decision
                                    // square, `write_w`/`write_h` carry the true
                                    // footprint into the mv stack, the motion-mode
                                    // row, the neighbour stamps and the rectangular
                                    // residual path (`rect_inter_residual_supported`
                                    // covers 16x8/8x16 since this round).
                                    let horz = part16 == PARTITION_HORZ;
                                    if horz {
                                        hit!(INTER_LEAF16_HORZ_HITS);
                                    } else {
                                        hit!(INTER_LEAF16_VERT_HITS);
                                    }
                                    let base_mi = sub16_to_mi(at16);
                                    let (ww, wh) = if horz { (16, 8) } else { (8, 16) };
                                    for piece in 0..2 {
                                        let piece_mi = if horz {
                                            (base_mi.0 + piece * 2, base_mi.1)
                                        } else {
                                            (base_mi.0, base_mi.1 + piece * 2)
                                        };
                                        // libaom `decode_partition`'s own second-
                                        // half frame-edge break.
                                        if piece > 0
                                            && (piece_mi.0 >= mi_rows as usize
                                                || piece_mi.1 >= mi_cols as usize)
                                        {
                                            bump_inter_edge_strip(if horz { 4 } else { 5 });
                                            break;
                                        }
                                        decode_inter_block(
                                            &mut dec,
                                            &mut cdfs,
                                            &mut neighbours,
                                            &mut grid,
                                            piece_mi,
                                            SUB,
                                            mi_cols,
                                            mi_rows,
                                            &mut y,
                                            &mut u,
                                            &mut v,
                                            refpix,
                                            &sign_bias_table,
                                            &global_motion,
                                            base_q_idx,
                                            TxbSet::Luma16,
                                            if reduced_tx_set {
                                                TxbSet::Luma16Inter
                                            } else {
                                                TxbSet::Luma16InterSet1
                                            },
                                            TxbSet::Chroma8,
                                            TX16,
                                            TX8,
                                            scan16,
                                            scan8,
                                            1,
                                            allow_high_precision_mv,
                                            force_integer_mv,
                                            interp_fixed,
                                            enable_dual_filter,
                                            tpl_frame.as_ref(),
                                            reference_select,
                                            enable_masked_compound,
                                            enable_interintra_compound,
                                            enable_jnt_comp,
                                            order_hint_bits,
                                            order_hint,
                                            ref_order_hints,
                                            skip_mode_present,
                                            skip_mode_frame,
                                            switchable_motion_mode,
                                            allow_warped_motion,
                                            true,
                                            ww,
                                            wh,
                                            allow_screen_content_tools,
                                            (frame_width as usize, frame_height as usize),
                                            fctx,
                                        )?;
                                    }
                                } else if (PARTITION_HORZ_A..=PARTITION_VERT_B).contains(&part16) {
                                    // lane-inter16ab r1: the four AB (T-shaped)
                                    // partitions at the 16x16 level of an INTER
                                    // frame -- two 8x8 inter leaves plus one true
                                    // 16x8/8x16 inter strip. libaom
                                    // `decode_partition` (decodeframe.c ~5040,
                                    // `bsize2 = get_partition_subsize(bsize,
                                    // PARTITION_SPLIT)` = BLOCK_8X8 here) visits:
                                    //   HORZ_A: TL 8x8, TR 8x8, bottom 16x8
                                    //   HORZ_B: top 16x8, BL 8x8, BR 8x8
                                    //   VERT_A: TL 8x8, BL 8x8, right 8x16
                                    //   VERT_B: left 8x16, TR 8x8, BR 8x8
                                    // Every piece is inside the frame by
                                    // construction: an AB value can only be read
                                    // from the (has_cols16 && has_rows16) branch
                                    // above, i.e. mi_row+2 < mi_rows and
                                    // mi_col+2 < mi_cols, which is exactly
                                    // libaom's own `has_rows`/`has_cols`.
                                    //
                                    // VERT_A/VERT_B visit TL,BL,TR,BR, so a
                                    // bottom-left square's top-right neighbour is
                                    // the partition's own not-yet-decoded
                                    // rectangle: libaom switches those squares
                                    // onto `has_tr_vert_*`/`has_bl_vert_*`
                                    // (`reconintra.c get_has_tr_table`) --
                                    // `Reach::vert_ab_partition()`, the same
                                    // guard the intra 16-level arm and the
                                    // 32-level inter AB arms take (class
                                    // `visit-order-changes-availability`).
                                    //
                                    // OBMC/warp and compound stay legal here:
                                    // every piece is >= 8x8, so libaom's
                                    // `is_motion_variation_allowed_bsize`
                                    // (min(bw,bh) >= 8, blockd.h) and
                                    // `is_comp_ref_allowed` (block_size_wide +
                                    // block_size_high >= 16, blockd.h) both hold
                                    // and the leaf/strip decoders' own gates
                                    // apply unchanged.
                                    bump_ab16_inter(part16 - PARTITION_HORZ_A);
                                    let _vert_guard = if part16 == PARTITION_VERT_A
                                        || part16 == PARTITION_VERT_B
                                    {
                                        Some(crate::encode::Reach::vert_ab_partition())
                                    } else {
                                        None
                                    };
                                    let (mi_row0, mi_col0) = sub16_to_mi(at16);
                                    let tl = (mi_row0, mi_col0);
                                    let tr = (mi_row0, mi_col0 + 2);
                                    let bl = (mi_row0 + 2, mi_col0);
                                    let br = (mi_row0 + 2, mi_col0 + 2);
                                    macro_rules! ab_strip16 {
                                        ($mi:expr, $ww:expr, $wh:expr) => {
                                            inter_piece!(
                                                $mi,
                                                SUB,
                                                TxbSet::Luma16,
                                                if reduced_tx_set {
                                                    TxbSet::Luma16Inter
                                                } else {
                                                    TxbSet::Luma16InterSet1
                                                },
                                                TxbSet::Chroma8,
                                                TX16,
                                                TX8,
                                                &scan16,
                                                &scan8,
                                                1,
                                                $ww,
                                                $wh
                                            )
                                        };
                                    }
                                    match part16 {
                                        PARTITION_HORZ_A => {
                                            inter_leaf8!(at16, tl);
                                            inter_leaf8!(at16, tr);
                                            ab_strip16!(bl, 16, 8);
                                        }
                                        PARTITION_HORZ_B => {
                                            ab_strip16!(tl, 16, 8);
                                            inter_leaf8!(at16, bl);
                                            inter_leaf8!(at16, br);
                                        }
                                        PARTITION_VERT_A => {
                                            inter_leaf8!(at16, tl);
                                            inter_leaf8!(at16, bl);
                                            ab_strip16!(tr, 8, 16);
                                        }
                                        _ => {
                                            ab_strip16!(tl, 8, 16);
                                            inter_leaf8!(at16, tr);
                                            inter_leaf8!(at16, br);
                                        }
                                    }
                                } else if part16 == PARTITION_HORZ_4 || part16 == PARTITION_VERT_4 {
                                    // lane-inter16ab r2: four 16x4 (4x16) INTER
                                    // strips. libaom `decode_partition`
                                    // (decodeframe.c, PARTITION_HORZ_4/VERT_4)
                                    // walks them top to bottom (left to right)
                                    // with its own `i > 0` frame-edge break.
                                    //
                                    // Each strip is >= 8 in one dimension only,
                                    // so `is_comp_ref_allowed`,
                                    // `is_motion_variation_allowed_bsize`
                                    // (blockd.h:65/1455, min(bw, bh) >= 8) and
                                    // `is_interintra_allowed_bsize` (BLOCK_8X8..
                                    // BLOCK_32X32 in enum order, which excludes
                                    // BLOCK_16X4 = 17) are all false: no
                                    // skip_mode, no comp_mode, no OBMC/warp and
                                    // no interintra symbol is read -- gated in
                                    // `decode_inter_block` off `write_w`/
                                    // `write_h`.
                                    //
                                    // CHROMA is the pair's:
                                    // `is_chroma_reference`
                                    // (av1_common_int.h:1454) is false on the
                                    // even strips, and the odd one codes one 8x4
                                    // (4x8) chroma transform at the pair's
                                    // origin, built from BOTH strips' motion
                                    // (`is_sub8x8_inter`).
                                    let horz = part16 == PARTITION_HORZ_4;
                                    let (mi_row0, mi_col0) = sub16_to_mi(at16);
                                    let (bw, bh) = if horz {
                                        (16usize, 4usize)
                                    } else {
                                        (4usize, 16usize)
                                    };
                                    for i in 0..4usize {
                                        let at_mi = if horz {
                                            (mi_row0 + i, mi_col0)
                                        } else {
                                            (mi_row0, mi_col0 + i)
                                        };
                                        if i > 0
                                            && (at_mi.0 >= mi_rows as usize
                                                || at_mi.1 >= mi_cols as usize)
                                        {
                                            break;
                                        }
                                        // lane-av1-444: at 4:4:4 every strip is
                                        // its own chroma reference
                                        // (`is_chroma_reference`: no mi parity
                                        // test at ss 0/0 and the strip's 16x4 /
                                        // 4x16 chroma block is already >= 4x4),
                                        // so there is no pair: each strip codes
                                        // and predicts its own chroma from its
                                        // own motion (`prev` = None).
                                        // lane-av1-422l: a strip pairs along its
                                        // OWN subsampled axis -- horz strips pair
                                        // on rows (no pair once ss_y == 0: chroma
                                        // has no row parity, every 16x4 strip is
                                        // its own reference, exactly the intra
                                        // strip arm's `else if ss_y(fctx) == 0`);
                                        // vert strips pair on columns (ss_x == 0
                                        // => every strip self-pairs, 4:2:2 keeps
                                        // the 4:2:0 column pairing -- a VERT_4
                                        // 4x16 strip's `ss_size_lookup` chroma
                                        // at ss (1,0) is BLOCK_INVALID, so only
                                        // the pair shape can ever be coded).
                                        let strip_has_chroma = if horz {
                                            i % 2 == 1 || ss_y(fctx) == 0
                                        } else {
                                            i % 2 == 1 || ss_x(fctx) == 0
                                        };
                                        let pair_mi = if !strip_has_chroma
                                            || (if horz {
                                                ss_y(fctx) == 0
                                            } else {
                                                ss_x(fctx) == 0
                                            }) {
                                            at_mi
                                        } else if horz {
                                            (at_mi.0 - 1, at_mi.1)
                                        } else {
                                            (at_mi.0, at_mi.1 - 1)
                                        };
                                        fctx.inter_strip_chroma.with(|c| {
                                            c.set(Some(InterStripChroma {
                                                has_chroma: strip_has_chroma,
                                                horz,
                                                pair_mi,
                                                prev: if strip_has_chroma
                                                    && (if horz {
                                                        ss_y(fctx) == 1
                                                    } else {
                                                        ss_x(fctx) == 1
                                                    }) {
                                                    fctx.inter_last_mc.with(std::cell::Cell::get)
                                                } else {
                                                    None
                                                },
                                            }))
                                        });
                                        inter_piece!(
                                            at_mi,
                                            SUB,
                                            TxbSet::Luma16,
                                            if reduced_tx_set {
                                                TxbSet::Luma16Inter
                                            } else {
                                                TxbSet::Luma16InterSet1
                                            },
                                            TxbSet::Chroma8,
                                            TX16,
                                            TX8,
                                            &scan16,
                                            &scan8,
                                            1,
                                            bw,
                                            bh
                                        );
                                        if horz {
                                            hit!(INTER16_HORZ4_HITS);
                                        } else {
                                            hit!(INTER16_VERT4_HITS);
                                        }
                                    }
                                } else if part16 != PARTITION_SPLIT {
                                    return Err(unsupported(
                                        "an inter 16x16-level partition value outside NONE/HORZ/VERT/SPLIT/AB/1:4",
                                    ));
                                } else {
                                    if has_cols16 && has_rows16 {
                                        hit!(INTER_SUB16_SPLIT_HITS);
                                    }
                                    let (mi_row0, mi_col0) =
                                        (sr as u32 * SUB_MI, sc as u32 * SUB_MI);
                                    let leaf_positions: Vec<(u32, u32)> = (0..4)
                                        .map(|i| (mi_row0 + (i / 2) * 2, mi_col0 + (i % 2) * 2))
                                        .filter(|&(mr, mc)| mr < mi_rows && mc < mi_cols)
                                        .collect();
                                    // lane-t900 r28: the 8x8-leaf refusal under a
                                    // scaled reference is LIFTED --
                                    // `decode_inter_block8`'s single-ref,
                                    // compound and OBMC MC all take spec
                                    // 7.11.3.3's scaled walk. The counter is what
                                    // proves a gate actually reached a scaled
                                    // leaf rather than a same-size reference.
                                    if (1..8).any(|rf| {
                                        refpix
                                            .dims(rf)
                                            .is_some_and(|(w, _)| w != frame_width as usize)
                                    }) {
                                        hit!(SCALED_LEAF8_HITS);
                                    }
                                    let mut prev_leaves: Vec<(
                                        (usize, usize),
                                        bool,
                                        bool,
                                        i8,
                                        Option<i8>,
                                    )> = Vec::new();
                                    for (mr, mc) in leaf_positions {
                                        let leaf_mi = (mr as usize, mc as usize);
                                        let leaf_ctx = neighbours.partition_ctx_mi(leaf_mi, 8);
                                        if crate::envflags::env_flag!("EC_TRACE_PART") {
                                            eprintln!(
                                                "EC_PART mi_row={} mi_col={} bsize=3 ctx={} rng={}",
                                                leaf_mi.0,
                                                leaf_mi.1,
                                                leaf_ctx,
                                                dec.debug_state().0
                                            );
                                        }
                                        let ec_dbg_pre = dec.debug_state().0;
                                        let part8 = dec.symbol(&mut cdfs.partition_w8[leaf_ctx]);
                                        // lane-av1unwritten r6: the partition symbol is RESOLVED and about to be
                                        // dispatched. `partition_subsize_dims` is libaom's `get_partition_subsize`
                                        // (common_data.h:71-90) reduced to dimensions, so this is
                                        // `decode_partition`'s pre-descend check (decodeframe.c:1451-1460) on EVERY
                                        // arm -- VERT, VERT_A / VERT_B, HORZ_4 and VERT_4 -- and the HORZ siblings
                                        // keep decoding because the helper is exact, not a heuristic. inter-frame 8x8 level.
                                        refuse_invalid_subsize((8, 8), part8, fctx)?;

                                        if std::env::var_os("EC_DBG_SUB8").is_some() {
                                            let ec_dbg_post = dec.debug_state().0;
                                            eprintln!(
                                                "EC_DBG b8 mi=({},{}) ctx={leaf_ctx} val={part8} pre={ec_dbg_pre} post={ec_dbg_post}",
                                                leaf_mi.0, leaf_mi.1
                                            );
                                        }
                                        if part8 == PARTITION_SPLIT {
                                            // lane-intersub8: four BLOCK_4X4 inter
                                            // sub-blocks, chroma once per 8x8.
                                            decode_inter_sub8_split4(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                &mut grid,
                                                leaf_mi,
                                                mi_cols,
                                                mi_rows,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                refpix,
                                                &sign_bias_table,
                                                &global_motion,
                                                base_q_idx,
                                                scan4,
                                                allow_high_precision_mv,
                                                force_integer_mv,
                                                interp_fixed,
                                                enable_dual_filter,
                                                tpl_frame.as_ref(),
                                                reduced_tx_set,
                                                (frame_width as usize, frame_height as usize),
                                                fctx,
                                            )?;
                                            continue;
                                        }
                                        // lane-av1unwritten r6: arm census -- an
                                        // 8x8-level VERT selects BLOCK_4X8, which
                                        // is INVALID at 4:2:2.
                                        note_subsize_arm(
                                            part8,
                                            chroma_plane_block_codable(
                                                partition_subsize_dims((8, 8), part8).0,
                                                partition_subsize_dims((8, 8), part8).1,
                                                ss_x(fctx),
                                                ss_y(fctx),
                                            ),
                                        );
                                        if part8 == PARTITION_VERT || part8 == PARTITION_HORZ {
                                            // lane-intersub8 r3/r4/r5: two
                                            // BLOCK_8X4 (HORZ) / BLOCK_4X8 (VERT)
                                            // inter leaves, chroma once per 8x8 on
                                            // the second of them. HORZ was refused
                                            // by name through r4 over a |d| <= 2
                                            // luma band; r5 merged lane-cdef
                                            // 8c6065e, whose `decode_inter_block8`
                                            // skip-band write is that band's root
                                            // cause, and the 56-arm sweep went 3
                                            // mismatching arms -> 0.
                                            decode_inter_sub8_rect2(
                                                &mut dec,
                                                &mut cdfs,
                                                &mut neighbours,
                                                &mut grid,
                                                leaf_mi,
                                                part8 == PARTITION_VERT,
                                                mi_cols,
                                                mi_rows,
                                                &mut y,
                                                &mut u,
                                                &mut v,
                                                refpix,
                                                &sign_bias_table,
                                                &global_motion,
                                                base_q_idx,
                                                scan4,
                                                allow_high_precision_mv,
                                                force_integer_mv,
                                                interp_fixed,
                                                enable_dual_filter,
                                                tpl_frame.as_ref(),
                                                reduced_tx_set,
                                                (frame_width as usize, frame_height as usize),
                                                fctx,
                                            )?;
                                            continue;
                                        }
                                        // `partition_w8` is a 4-symbol alphabet
                                        // (NONE/HORZ/VERT/SPLIT), all four handled.
                                        let (
                                            skip,
                                            is_inter,
                                            skip_mode_leaf,
                                            compound_ctx8,
                                            leaf_filter,
                                            leaf_refs,
                                        ) = decode_inter_block8(
                                            &mut dec,
                                            &mut cdfs,
                                            &mut neighbours,
                                            &mut grid,
                                            mi_cols,
                                            mi_rows,
                                            at16,
                                            leaf_mi,
                                            &mut y,
                                            &mut u,
                                            &mut v,
                                            refpix,
                                            base_q_idx,
                                            scan8,
                                            scan4,
                                            allow_high_precision_mv,
                                            force_integer_mv,
                                            &global_motion,
                                            enable_dual_filter,
                                            reference_select,
                                            enable_masked_compound,
                                            enable_interintra_compound,
                                            enable_jnt_comp,
                                            order_hint_bits,
                                            order_hint,
                                            ref_order_hints,
                                            skip_mode_present,
                                            skip_mode_frame,
                                            reduced_tx_set,
                                            interp_fixed,
                                            switchable_motion_mode,
                                            allow_warped_motion,
                                            allow_screen_content_tools,
                                            &sign_bias_table,
                                            tpl_frame.as_ref(),
                                            (frame_width as usize, frame_height as usize),
                                            fctx,
                                        )?;
                                        prev_leaves.push((
                                            leaf_mi,
                                            skip,
                                            is_inter,
                                            leaf_refs.0,
                                            leaf_refs.1,
                                        ));
                                        // lane-inter8 r2: stamp THIS leaf's own
                                        // 2x2-mi span before the next leaf reads
                                        // it as a neighbour -- the whole-16x16
                                        // stamp below only ever described the
                                        // last leaf, so three quarters of the
                                        // block carried the wrong skip/ref/inter
                                        // state into the next block's contexts
                                        // (class context-read-from-one-cell).
                                        // lane-gmaffine r3: with the mi-granular
                                        // band, the leaf's OWN switchable-filter
                                        // symbols go in here -- the `[3, 3]`
                                        // ("intra, no filter") sentinel this used
                                        // to stamp made `obmc_blend`'s
                                        // `neighbour_filter` PANIC as soon as a
                                        // sibling leaf blended one of these.
                                        neighbours.record_inter_rect_mi(
                                            leaf_mi,
                                            2,
                                            2,
                                            skip,
                                            is_inter,
                                            leaf_refs.0,
                                            leaf_filter,
                                            skip_mode_leaf,
                                        );
                                        if let Some(ref1) = leaf_refs.1
                                            && let Some((_, _, group_idx, idx)) = compound_ctx8
                                        {
                                            neighbours.record_compound_ctx_rect_mi(
                                                leaf_mi, 2, 2, ref1, group_idx, idx,
                                            );
                                        }
                                    }
                                    // lane-inter8 r2: the four per-leaf
                                    // `record_inter_rect_mi` stamps above replace
                                    // the single whole-16x16 one that used to run
                                    // here with the LAST leaf's state.
                                }
                            }
                        }
                        PARTITION_HORZ_B => {
                            // lane-warp r5: PARTITION_HORZ_B = 32x32 top strip +
                            // 16x16 bottom-left + 16x16 bottom-right. aomenc
                            // carves static regions into it; the strip is
                            // decoded as a square 32x32 block: for a `skip`
                            // block the symbol stream is identical to the true
                            // 32x16 coding (no residual, no motion_mode/warp
                            // symbol), and the square stamps it leaves over the
                            // bottom half (neighbour arrays, skip/lf grids) are
                            // re-stamped by the C/D leaves right after. The
                            // mi-granular `left_side_mi` rows of a last-block-in-
                            // tile strip leak 32-vs-16 only until the per-tile
                            // `Neighbours::new` reset.
                            hit!(EXTENDED_PARTITION_HITS);
                            let at32 = at;
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi(at32),
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                scan32,
                                scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                true,
                                // lane-rect r2: the top strip's TRUE 32x16
                                // footprint (was BLOCK,BLOCK -- the same square
                                // corner-cut rect-flake-1 exposed on HORZ).
                                BLOCK,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            if (r32 * 2 + 1) * SUB_MI < mi_rows {
                                decode_inter_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    &mut grid,
                                    sub16_to_mi((r32 as usize * 2 + 1, c32 as usize * 2)),
                                    SUB,
                                    mi_cols,
                                    mi_rows,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    refpix,
                                    &sign_bias_table,
                                    &global_motion,
                                    base_q_idx,
                                    TxbSet::Luma16,
                                    if reduced_tx_set {
                                        TxbSet::Luma16Inter
                                    } else {
                                        TxbSet::Luma16InterSet1
                                    },
                                    TxbSet::Chroma8,
                                    TX16,
                                    TX8,
                                    scan16,
                                    scan8,
                                    2,
                                    allow_high_precision_mv,
                                    force_integer_mv,
                                    interp_fixed,
                                    enable_dual_filter,
                                    tpl_frame.as_ref(),
                                    reference_select,
                                    enable_masked_compound,
                                    enable_interintra_compound,
                                    enable_jnt_comp,
                                    order_hint_bits,
                                    order_hint,
                                    ref_order_hints,
                                    skip_mode_present,
                                    skip_mode_frame,
                                    switchable_motion_mode,
                                    allow_warped_motion,
                                    false,
                                    SUB,
                                    SUB,
                                    allow_screen_content_tools,
                                    (frame_width as usize, frame_height as usize),
                                    fctx,
                                )?;
                                if (c32 * 2 + 1) * SUB_MI < mi_cols {
                                    decode_inter_block(
                                        &mut dec,
                                        &mut cdfs,
                                        &mut neighbours,
                                        &mut grid,
                                        sub16_to_mi((r32 as usize * 2 + 1, c32 as usize * 2 + 1)),
                                        SUB,
                                        mi_cols,
                                        mi_rows,
                                        &mut y,
                                        &mut u,
                                        &mut v,
                                        refpix,
                                        &sign_bias_table,
                                        &global_motion,
                                        base_q_idx,
                                        TxbSet::Luma16,
                                        if reduced_tx_set {
                                            TxbSet::Luma16Inter
                                        } else {
                                            TxbSet::Luma16InterSet1
                                        },
                                        TxbSet::Chroma8,
                                        TX16,
                                        TX8,
                                        scan16,
                                        scan8,
                                        2,
                                        allow_high_precision_mv,
                                        force_integer_mv,
                                        interp_fixed,
                                        enable_dual_filter,
                                        tpl_frame.as_ref(),
                                        reference_select,
                                        enable_masked_compound,
                                        enable_interintra_compound,
                                        enable_jnt_comp,
                                        order_hint_bits,
                                        order_hint,
                                        ref_order_hints,
                                        skip_mode_present,
                                        skip_mode_frame,
                                        switchable_motion_mode,
                                        allow_warped_motion,
                                        false,
                                        SUB,
                                        SUB,
                                        allow_screen_content_tools,
                                        (frame_width as usize, frame_height as usize),
                                        fctx,
                                    )?;
                                }
                            }
                        }
                        PARTITION_HORZ => {
                            // lane-rect r2: two true 32x16 strips. Both read at
                            // `side=BLOCK` (CDF/size-class selection stays square,
                            // matching HORZ_B's accepted corner-cut) but bw4/bh4
                            // (mvstack.rs, `motion_mode_eligible`) now derive from
                            // `write_w`/`write_h`'s true 32x16 footprint -- the
                            // pin's fix (r1's finding).
                            hit!(RECT_PARTITION_HITS);
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi(at),
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                scan32,
                                scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                true,
                                BLOCK,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            if !has_rows32 {
                                bump_edge32(2);
                            }
                            // lane-golomb r2: at the frame edge only the first half of the
                            // strip is coded (libaom `decode_partition` guards the second
                            // with the same has_rows32 test that made this partition non-square).
                            if has_rows32 {
                                decode_inter_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    &mut grid,
                                    sub16_to_mi((r32 as usize * 2 + 1, c32 as usize * 2)),
                                    BLOCK,
                                    mi_cols,
                                    mi_rows,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    refpix,
                                    &sign_bias_table,
                                    &global_motion,
                                    base_q_idx,
                                    TxbSet::Luma32,
                                    TxbSet::Luma32Inter,
                                    TxbSet::Chroma16,
                                    TX32,
                                    TX16,
                                    scan32,
                                    scan16,
                                    3,
                                    allow_high_precision_mv,
                                    force_integer_mv,
                                    interp_fixed,
                                    enable_dual_filter,
                                    tpl_frame.as_ref(),
                                    reference_select,
                                    enable_masked_compound,
                                    enable_interintra_compound,
                                    enable_jnt_comp,
                                    order_hint_bits,
                                    order_hint,
                                    ref_order_hints,
                                    skip_mode_present,
                                    skip_mode_frame,
                                    switchable_motion_mode,
                                    allow_warped_motion,
                                    true,
                                    BLOCK,
                                    SUB,
                                    allow_screen_content_tools,
                                    (frame_width as usize, frame_height as usize),
                                    fctx,
                                )?;
                            } else {
                                bump_inter_edge_strip(2);
                            }
                        }
                        PARTITION_VERT => {
                            // lane-rect r2: mirror of PARTITION_HORZ above with
                            // width/height swapped.
                            hit!(RECT_PARTITION_HITS);
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi(at),
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                scan32,
                                scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                true,
                                SUB,
                                BLOCK,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            if !has_cols32 {
                                bump_edge32(3);
                            }
                            // lane-golomb r2: at the frame edge only the first half of the
                            // strip is coded (libaom `decode_partition` guards the second
                            // with the same has_cols32 test that made this partition non-square).
                            if has_cols32 {
                                decode_inter_block(
                                    &mut dec,
                                    &mut cdfs,
                                    &mut neighbours,
                                    &mut grid,
                                    sub16_to_mi((r32 as usize * 2, c32 as usize * 2 + 1)),
                                    BLOCK,
                                    mi_cols,
                                    mi_rows,
                                    &mut y,
                                    &mut u,
                                    &mut v,
                                    refpix,
                                    &sign_bias_table,
                                    &global_motion,
                                    base_q_idx,
                                    TxbSet::Luma32,
                                    TxbSet::Luma32Inter,
                                    TxbSet::Chroma16,
                                    TX32,
                                    TX16,
                                    scan32,
                                    scan16,
                                    3,
                                    allow_high_precision_mv,
                                    force_integer_mv,
                                    interp_fixed,
                                    enable_dual_filter,
                                    tpl_frame.as_ref(),
                                    reference_select,
                                    enable_masked_compound,
                                    enable_interintra_compound,
                                    enable_jnt_comp,
                                    order_hint_bits,
                                    order_hint,
                                    ref_order_hints,
                                    skip_mode_present,
                                    skip_mode_frame,
                                    switchable_motion_mode,
                                    allow_warped_motion,
                                    true,
                                    SUB,
                                    BLOCK,
                                    allow_screen_content_tools,
                                    (frame_width as usize, frame_height as usize),
                                    fctx,
                                )?;
                            } else {
                                bump_inter_edge_strip(3);
                            }
                        }
                        PARTITION_HORZ_A => {
                            // lane-partab r1: two 16x16 squares on top + a true
                            // 32x16 strip below (libaom decode_partition
                            // PARTITION_HORZ_A: TL, TR, bottom strip).
                            hit!(PARTAB_HITS);
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi(at),
                                SUB,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma16,
                                if reduced_tx_set {
                                    TxbSet::Luma16Inter
                                } else {
                                    TxbSet::Luma16InterSet1
                                },
                                TxbSet::Chroma8,
                                TX16,
                                TX8,
                                scan16,
                                scan8,
                                2,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                SUB,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi((r32 as usize * 2, c32 as usize * 2 + 1)),
                                SUB,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma16,
                                if reduced_tx_set {
                                    TxbSet::Luma16Inter
                                } else {
                                    TxbSet::Luma16InterSet1
                                },
                                TxbSet::Chroma8,
                                TX16,
                                TX8,
                                scan16,
                                scan8,
                                2,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                SUB,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi((r32 as usize * 2 + 1, c32 as usize * 2)),
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                scan32,
                                scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                true,
                                BLOCK,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                        }
                        PARTITION_VERT_A => {
                            // lane-partab r1: mirror of PARTITION_HORZ_A with
                            // width/height swapped (TL, BL, right 16x32 strip).
                            hit!(PARTAB_HITS);
                            let _vert_ab = crate::encode::Reach::vert_ab_partition();
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi(at),
                                SUB,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma16,
                                if reduced_tx_set {
                                    TxbSet::Luma16Inter
                                } else {
                                    TxbSet::Luma16InterSet1
                                },
                                TxbSet::Chroma8,
                                TX16,
                                TX8,
                                scan16,
                                scan8,
                                2,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                SUB,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi((r32 as usize * 2 + 1, c32 as usize * 2)),
                                SUB,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma16,
                                if reduced_tx_set {
                                    TxbSet::Luma16Inter
                                } else {
                                    TxbSet::Luma16InterSet1
                                },
                                TxbSet::Chroma8,
                                TX16,
                                TX8,
                                scan16,
                                scan8,
                                2,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                SUB,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi((r32 as usize * 2, c32 as usize * 2 + 1)),
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                scan32,
                                scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                true,
                                SUB,
                                BLOCK,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                        }
                        PARTITION_VERT_B => {
                            // lane-partab r1: true 16x32 strip on the left + two
                            // 16x16 squares on the right (libaom decode_partition
                            // PARTITION_VERT_B: left strip, TR, BR).
                            hit!(PARTAB_HITS);
                            let _vert_ab = crate::encode::Reach::vert_ab_partition();
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi(at),
                                BLOCK,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma32,
                                TxbSet::Luma32Inter,
                                TxbSet::Chroma16,
                                TX32,
                                TX16,
                                scan32,
                                scan16,
                                3,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                true,
                                SUB,
                                BLOCK,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi((r32 as usize * 2, c32 as usize * 2 + 1)),
                                SUB,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma16,
                                if reduced_tx_set {
                                    TxbSet::Luma16Inter
                                } else {
                                    TxbSet::Luma16InterSet1
                                },
                                TxbSet::Chroma8,
                                TX16,
                                TX8,
                                scan16,
                                scan8,
                                2,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                SUB,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                            decode_inter_block(
                                &mut dec,
                                &mut cdfs,
                                &mut neighbours,
                                &mut grid,
                                sub16_to_mi((r32 as usize * 2 + 1, c32 as usize * 2 + 1)),
                                SUB,
                                mi_cols,
                                mi_rows,
                                &mut y,
                                &mut u,
                                &mut v,
                                refpix,
                                &sign_bias_table,
                                &global_motion,
                                base_q_idx,
                                TxbSet::Luma16,
                                if reduced_tx_set {
                                    TxbSet::Luma16Inter
                                } else {
                                    TxbSet::Luma16InterSet1
                                },
                                TxbSet::Chroma8,
                                TX16,
                                TX8,
                                scan16,
                                scan8,
                                2,
                                allow_high_precision_mv,
                                force_integer_mv,
                                interp_fixed,
                                enable_dual_filter,
                                tpl_frame.as_ref(),
                                reference_select,
                                enable_masked_compound,
                                enable_interintra_compound,
                                enable_jnt_comp,
                                order_hint_bits,
                                order_hint,
                                ref_order_hints,
                                skip_mode_present,
                                skip_mode_frame,
                                switchable_motion_mode,
                                allow_warped_motion,
                                false,
                                SUB,
                                SUB,
                                allow_screen_content_tools,
                                (frame_width as usize, frame_height as usize),
                                fctx,
                            )?;
                        }
                        PARTITION_HORZ_4 | PARTITION_VERT_4 => {
                            // lane-inter4 r1: four 32x8 / 8x32 inter strips,
                            // `quarter_step` = 2 mi -- the arm ReelC's first inter
                            // frame stops at (`value=9` = PARTITION_VERT_4).
                            // `at` is in 16-px cells and cannot name an 8-px
                            // offset, which is why `decode_inter_block` takes mi
                            // coordinates now.
                            let horz = part32 == PARTITION_HORZ_4;
                            let base_mi = (at.0 * (SUB / MI), at.1 * (SUB / MI));
                            let (bw, bh) = if horz { (32usize, 8usize) } else { (8, 32) };
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
                                inter_piece!(
                                    strip_mi,
                                    BLOCK,
                                    TxbSet::Luma32,
                                    TxbSet::Luma32Inter,
                                    TxbSet::Chroma16,
                                    TX32,
                                    TX16,
                                    &scan32,
                                    &scan16,
                                    3,
                                    bw,
                                    bh
                                );
                                if horz {
                                    hit!(RECT4_32_HORZ_INTER_HITS);
                                } else {
                                    hit!(RECT4_32_VERT_INTER_HITS);
                                }
                            }
                        }
                        _ => {
                            return Err(unsupported(format!(
                                "an INTER 32x32 partition type this decoder does not code (value={part32})"
                            )));
                        }
                    }
                }
            }
        }
        wave::finish_tile(fctx, refpix, [&mut y, &mut u, &mut v]);
        // lane-timeline: this tile's last symbol is read here.
        crate::timeline::tile_done();

        if tile_num == tile_info.context_update_tile_id {
            result_cdfs = cdfs;
        }
    }
    refpix.take_err()?;
    crate::timeline::recon_done();
    // lane-twostage: the motion field is built from the mode-info grid alone
    // (no pixel), so it moves up here with the CDFs -- everything a
    // consuming frame's SETUP needs, published before this frame's filters.
    let mut motion_field = Some(build_motion_field(
        &grid,
        mi_cols as usize,
        mi_rows as usize,
        order_hint,
        order_hint_bits,
        ref_order_hints,
        fctx,
    ));
    if let Some(stage1) = stage1
        && superres(fctx).is_none()
    {
        stage1(
            result_cdfs.clone(),
            motion_field
                .take()
                .expect("the motion field was just built"),
            frame_width as usize,
            frame_height as usize,
        );
    }

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
        // lane-av1dumploud: loud on a short write (see the key-frame twin
        // above -- same file, same shape, same three planes).
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
    // lane-av1444edge: the inter-frame path's mi-aligned pre-loop-filter
    // dump, the rung the key-frame path has always had (see
    // `dump_prefilter_wide`). Placed beside the padded `EC_AV1_PREFILT_DUMP`
    // above, i.e. before the loop filters run, so the two agree on the point
    // in the sequence they sample.
    dump_prefilter_wide(&y, &u, &v);
    // lane-av1fwd: the encoder scores ~25 filter candidates against this
    // same coded tile ([`FilterReplay`]). Capture the reconstruction here,
    // before either filter has touched it -- but only on the shape whose
    // tail is exactly `apply_deblock` + `apply_cdef` and nothing else.
    if !lr_active && superres(fctx).is_none() {
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
    let deblocked = lr_active.then(|| {
        (
            plane_snapshot(&y, 0),
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
            motion_field,
        ));
    }
    let crop = |plane: &PlaneBuf<'_>, w: usize, h: usize| -> Vec<u16> {
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            out.extend(plane.data[row * plane.width..][..w].iter().copied());
        }
        out
    };
    // lane-superres r10 (fixed r9): same real-margin stash as
    // `decode_key_frame_tile_with_cdfs` (see that function's comment) -- the
    // real reconstructed extent is the mi-aligned `true_width`/`true_height`
    // (`mi_cols`/`mi_rows` * 4), NOT the superblock-padded `width`/`height`
    // this branch was reading before: columns `[true_width, width)` were
    // never actually coded (the last partial superblock stops at
    // `true_width`), so stashing out to `width` handed the chroma upscale's
    // right-edge replicate a slice of uninitialized/stale buffer one column
    // early -- the same shorten-vs-replicate shape as `av1-truesize`. The
    // key-frame branch above already used `true_width`/`true_height`; this
    // just matches it.
    fctx.last_frame_wide_margin.with(|m| {
        *m.borrow_mut() = if true_width > fw || true_height > fh {
            Some(Picture {
                width: true_width,
                height: true_height,
                y: crop(&y, true_width, true_height),
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
        motion_field,
    ))
}

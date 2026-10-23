//! Inter-block context and reference helpers: switchable interp and intra/inter contexts, the OBMC run, compound ref and mv readers. Moved out of `decode/mod.rs` (av1split D9); the items are unchanged apart from `pub(super)` visibility.

use super::*;

/// `av1_get_pred_context_switchable_interp`'s context for `interp_filter[dir]`
/// -- `above`/`left` are the neighbour's own resolved index for this
/// direction (0..=2, or `3` = no info, spec `SWITCHABLE_FILTERS`).
/// `is_compound` folds in libaom `av1_get_pred_context_switchable_interp`'s
/// own `ctx_offset` term (`INTER_FILTER_COMP_OFFSET` = 4, added when *this*
/// block itself has a second reference) -- lane-av1idx: a stale "this
/// decoder never codes a compound reference" assumption baked this to
/// always-0, which desynced the CDF context bucket (not the decoded value
/// outright, hence small pixel deltas rather than a hard desync) the moment
/// lane-av1blend unmasked plain-average compound blocks on a Switchable-
/// filter frame.
pub(super) fn switchable_interp_ctx(above: u8, left: u8, dir: usize, is_compound: bool) -> usize {
    let base = dir * 8 + if is_compound { 4 } else { 0 };
    let term = if left == above {
        left
    } else if left == 3 {
        above
    } else if above == 3 {
        left
    } else {
        3
    };
    base + term as usize
}

/// Resolves the interpolation filter kernel(s) an inter block's motion
/// compensation reads (spec 5.11.10/5.11.20): a fixed-filter frame
/// (`interp_fixed = Some`) never reads a symbol; a `Switchable` frame
/// (`interp_fixed = None`) reads `interp_filter[0]` and, when
/// `enable_dual_filter`, a second `interp_filter[1]` -- unless
/// `force_regular` (this block's own `needs_interp_filter() == 0` case: a
/// `GLOBALMV` block, whose global motion is always `IDENTITY` in this
/// decoder's scope, spec 5.11.10's `large && YMode == GLOBALMV` branch),
/// which forces `EIGHTTAP` without reading anything.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_interp_filter(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    interp_fixed: Option<mc::InterpFilterKind>,
    enable_dual_filter: bool,
    force_regular: bool,
    above: [u8; 2],
    left: [u8; 2],
    is_compound: bool,
) -> (mc::InterpFilterKind, mc::InterpFilterKind, [u8; 2]) {
    if let Some(kind) = interp_fixed {
        return (kind, kind, [3, 3]);
    }
    if force_regular {
        return (
            mc::InterpFilterKind::Regular,
            mc::InterpFilterKind::Regular,
            [0, 0],
        );
    }
    // libaom `av1_extract_interp_filter`/`read_mb_interp_filter`: the
    // symbol read at `dir=0` becomes the block's *vertical* (`y_filter`)
    // kernel, `dir=1` the *horizontal* (`x_filter`) one -- `dir` is not
    // "horizontal then vertical" despite `predict_with_filters`' own
    // `h_kind`/`v_kind` argument order, which is why this function returns
    // `(h, v, ..)` but reads `dir0` (`v`) before `dir1` (`h`).
    let ctx0 = switchable_interp_ctx(above[0], left[0], 0, is_compound);
    if crate::envflags::env_flag!("EC_AV1_IFDBG") {
        eprintln!(
            "IFDBG dir=0 ctx={ctx0} above={above:?} left={left:?} cdf={:?}",
            &cdfs.switchable_interp[ctx0][..3]
        );
    }
    let sym0 = dec.symbol(&mut cdfs.switchable_interp[ctx0]) as u8;
    let sym1 = if enable_dual_filter {
        let ctx1 = switchable_interp_ctx(above[1], left[1], 1, is_compound);
        if crate::envflags::env_flag!("EC_AV1_IFDBG") {
            eprintln!(
                "IFDBG dir=1 ctx={ctx1} above={above:?} left={left:?} cdf={:?} sym0={sym0}",
                &cdfs.switchable_interp[ctx1][..3]
            );
        }
        dec.symbol(&mut cdfs.switchable_interp[ctx1]) as u8
    } else {
        sym0
    };
    if enable_dual_filter && sym0 != sym1 {
        hit!(DUAL_FILTER_DIFF_HITS);
    }
    if crate::envflags::env_flag!("EC_TRACE_MODE") {
        eprintln!(
            "EC_IF ctx0={ctx0} above={above:?} left={left:?} sym0={sym0} sym1={sym1} rng={}",
            dec.debug_state().0
        );
    }
    (
        mc::InterpFilterKind::from_switchable_symbol(sym1 as usize),
        mc::InterpFilterKind::from_switchable_symbol(sym0 as usize),
        [sym0, sym1],
    )
}

/// The `is_inter` context (spec 5.11.16 via `av1_get_intra_inter_context`),
/// duplicating [`crate::tile`]'s private copy of the same rule.
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

/// `CLASS0_SIZE << (class + 2)`, duplicating `crate::tile`'s private
/// `mv_class_base` (spec 3): the magnitude an `MV_CLASS_n` component's own
/// bits start counting from.
fn mv_class_base(class: usize) -> i32 {
    if class == 0 { 0 } else { 2i32 << (class + 2) }
}

/// One motion vector component's non-zero diff (spec 5.11.32
/// `read_mv_component`), the inverse of [`crate::tile`]'s private
/// `write_mv_component`.
fn read_mv_component(
    dec: &mut SymbolDecoder,
    c: &mut MvComponentCdfs,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
) -> i32 {
    let sign = dec.symbol(&mut c.sign);
    let class = dec.symbol(&mut c.class);
    let local = if class == 0 {
        let bit = dec.symbol(&mut c.class0_bit);
        // spec 5.11.32 `mv_class0_fr`: not coded at all when the frame
        // forces integer motion vectors -- implicitly `3` (the top
        // fractional value) rather than read, else every symbol after it
        // in the tile desyncs by one read.
        let fr = if force_integer_mv {
            3
        } else {
            dec.symbol(&mut c.class0_fr[bit])
        };
        // spec 5.11.32 `mv_class0_hp`: read only when the frame allows
        // high-precision motion vectors, else implicitly 1 (the low bit
        // stays at half-pel precision) -- this crate's own writer always
        // leaves the flag off, but a foreign stream can set it per frame.
        let hp = if allow_high_precision_mv {
            dec.symbol(&mut c.class0_hp)
        } else {
            1
        };
        (bit << 3) | (fr << 1) | hp
    } else {
        let mut d = 0;
        for i in 0..class {
            d |= dec.symbol(&mut c.bit[i]) << i;
        }
        // spec 5.11.32 `mv_fr`: same force_integer_mv carve-out as
        // `mv_class0_fr` above.
        let fr = if force_integer_mv {
            3
        } else {
            dec.symbol(&mut c.fr)
        };
        let hp = if allow_high_precision_mv {
            dec.symbol(&mut c.hp)
        } else {
            1
        };
        (d << 3) | (fr << 1) | hp
    };
    let mag = mv_class_base(class) + local as i32 + 1;
    if sign == 1 { -mag } else { mag }
}

/// A motion vector coded as a residual against `pred` (spec 5.11.32
/// `read_mv`), the inverse of [`crate::tile`]'s private `write_mv`.
pub(super) fn read_mv(
    dec: &mut SymbolDecoder,
    mv_comp: &mut [MvComponentCdfs; 2],
    mv_joint: &mut [u16; 5],
    pred: (i32, i32),
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    fctx: &crate::decode::FrameCtx,
) -> (i32, i32) {
    let joint = dec.symbol(mv_joint);
    let mut diff = (0, 0);
    if joint == 2 || joint == 3 {
        diff.0 = read_mv_component(
            dec,
            &mut mv_comp[0],
            allow_high_precision_mv,
            force_integer_mv,
        );
    }
    if joint == 1 || joint == 3 {
        diff.1 = read_mv_component(
            dec,
            &mut mv_comp[1],
            allow_high_precision_mv,
            force_integer_mv,
        );
    }
    let mut mv = (pred.0 + diff.0, pred.1 + diff.1);
    // Spec 6.10.25 (`is_mv_valid`): every component is strictly inside
    // (MV_LOW, MV_UPP) = (-(1 << 14), 1 << 14). A conforming stream never leaves
    // that range. Past it the vector would not fit the i16 storage of a mi unit,
    // so flag the frame corrupt and clamp it into range for the rest of the decode.
    const MV_LIMIT: i32 = 1 << 14;
    let valid = |c: i32| c > -MV_LIMIT && c < MV_LIMIT;
    if !valid(mv.0) || !valid(mv.1) {
        fctx.flag_corrupt("motion vector outside the spec's range");
        mv = (
            mv.0.clamp(-MV_LIMIT + 1, MV_LIMIT - 1),
            mv.1.clamp(-MV_LIMIT + 1, MV_LIMIT - 1),
        );
    }
    crate::census::mv(mv.0, mv.1);
    mv
}

/// One intrabc block's block vector (spec 5.11.13 `read_intrabc_info` ->
/// `assign_mv` with `use_intrabc`, libaom `assign_dv`/`av1_find_ref_dv`).
///
/// The predictor is the ordinary MV stack built against `INTRA_FRAME`
/// (`av1_find_mv_refs(..., INTRA_FRAME, ...)`): `nearestmv`, or `nearmv`
/// when that is zero, or -- when both are -- the fixed fallback DV one
/// superblock up, or `INTRABC_DELAY_PIXELS` (256) plus one superblock to the
/// left when this is the tile's first superblock row. The predictor is then
/// rounded to full pel, and the difference reads off the *dv* nmv context
/// (libaom `ndvc`) at full-pel precision (`MV_SUBPEL_NONE`: no `fr`/`hp`
/// symbols, exactly this decoder's `force_integer_mv` carve-out). No DRL
/// symbol is coded for a DV, so none of this can move the bitstream
/// position -- only the reconstructed vector.
pub(super) fn read_intrabc_dv(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    mi_r: usize,
    mi_c: usize,
    // The block's own mi footprint (`av1_find_mv_refs` takes the real bsize,
    // so a sub-8x8 `BLOCK_4X8`/`BLOCK_8X4` leaf scans a 1x2/2x1 neighbourhood
    // -- lane-av1txr).
    (n4_w, n4_h): (usize, usize),
    fctx: &crate::decode::FrameCtx,
) -> (i32, i32) {
    const INTRABC_DELAY_PIXELS: i32 = 256;
    let stack_pred = fctx.intrabc_mi_grid.with(|g| {
        let g = g.borrow();
        g.as_ref().map(|(grid, mi_cols, mi_rows)| {
            let stack = crate::mvstack::find_mv_stack(
                grid, mi_r, mi_c, n4_w, n4_h, 0, /* INTRA_FRAME */
                *mi_cols, *mi_rows,
            );
            if stack.nearest_mv == (0, 0) {
                stack.near_mv
            } else {
                stack.nearest_mv
            }
        })
    });
    let mut pred = stack_pred.unwrap_or((0, 0));
    if pred == (0, 0) {
        // `av1_find_ref_dv` (mvref_common.c). Tile row start is 0 here (the
        // single-tile-row case every intrabc gate stream is).
        // lane-t900 r34: `av1_find_ref_dv` takes `mib_size` -- the SEQUENCE's
        // superblock size in mi units -- so a 128x128 superblock stream's
        // fallback DV is one 128-px superblock up (or 128+256 px left on the
        // tile's first superblock row), never the 64 this hardcoded.
        let sb_mi = sb_mi_cur(fctx) as i32;
        let sb_px = sb_mi * MI as i32;
        pred = if (mi_r as i32) < sb_mi {
            (0, -(sb_px + INTRABC_DELAY_PIXELS) * 8)
        } else {
            (-sb_px * 8, 0)
        };
    }
    // "Ref DV should not have sub-pel" (`assign_dv`): floor to full pel.
    let pred = ((pred.0 >> 3) * 8, (pred.1 >> 3) * 8);
    let dv = read_mv(
        dec,
        &mut cdfs.dv_comp,
        &mut cdfs.dv_joint,
        pred,
        false,
        true,
        fctx,
    );
    if crate::envflags::env_flag!("EC_TRACE_MODE_STEP") {
        let (rng, _) = dec.debug_state();
        eprintln!(
            "EC_DV mi_row={mi_r} mi_col={mi_c} dv_col={} dv_row={} rng={rng}",
            dv.1, dv.0
        );
    }
    dv
}

/// A 1/8-pel motion vector component converted to the 1/16-pel offset
/// [`mc::predict`] takes, duplicating [`crate::encode`]'s private `mv_to_q4`
/// (its own doc comment explains the `*2`/`*1` luma/chroma split).
pub(super) fn mv_to_q4(pos: usize, mv_component: i32, ss: usize) -> i32 {
    // orig_pos += mv * (1 << (1 - ss)). Luma always passes 0.
    (pos as i32) * 16 + mv_component * (1i32 << (1 - ss as i32))
}

impl PlaneBuf<'_> {
    /// Adds `residual` onto an already-computed `prediction` (motion
    /// compensation's output, rather than [`predict`]'s intra one), writing
    /// the clamped reconstruction into the plane at `(x, y)` -- the inter
    /// counterpart of [`PlaneBuf::reconstruct`].
    /// [`Self::reconstruct_mc_rect`], writing only a `w`x`h` sub-rectangle of the
    /// `side`x`side` `prediction`/`residual` buffers (which stay `side`-square
    /// for stride/indexing) -- lane-partitions r1: a true rectangular HORZ/
    /// VERT strip is predicted at its enclosing square `side` (matching
    /// `HORZ_B`'s already-accepted square-context corner-cut, lane-warp r5)
    /// but only its own true `w`x`h` footprint is committed to the plane, so
    /// two strips sharing a square's coordinate origin never clobber each
    /// other's real pixels.
    #[track_caller]
    // Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn reconstruct_mc_rect(
        &mut self,
        x: usize,
        y: usize,
        plane: usize,
        side: usize,
        w: usize,
        h: usize,
        prediction: &[u16],
        residual: &[i32],
        fctx: &crate::decode::FrameCtx,
    ) {
        // lane-sb128c r7: a 128x64 half of a 128 root at the BOTTOM frame edge
        // is the first block shape whose true footprint overhangs the padded
        // coding surface (mi_rows = 198 -> a plane 800 rows tall, block rows
        // 768..832). libaom reconstructs those rows into the frame buffer's
        // border; nothing reads them back, so they are simply dropped here.
        let (h, w) = (
            h.min(self.height.saturating_sub(y)),
            w.min(self.width.saturating_sub(x)),
        );
        // lane-perf4: one row of each operand at a time. `side` is the
        // prediction/residual stride and `self.width` the plane's, so the
        // three row bases are computed once per row instead of a
        // multiply-add per sample (the samples themselves are unchanged).
        let max = crate::decode::sample_max(fctx);

        // lane-av1ibcfork: the MC/inter write's own origin and shape. The intra
        // side's `EC_PRED` rung prints the PREDICTION, so a region whose
        // prediction is byte-identical to the oracle's and whose committed
        // samples are not can only be a DIFFERENT WRITE -- and this is the
        // only write path with no rung of its own.
        if crate::envflags::env_flag!("EC_MCWR") {
            eprintln!(
                "EC_MCWR x={x} y={y} side={side} w={w} h={h} empty_res={} at {}",
                residual.is_empty(),
                std::panic::Location::caller()
            );
        }
        // lane-av1444altref r2: EC_CPRED, the picture-independent twin of the
        // oracle's rung of the same name (aomdec's
        // `dec_build_inter_predictor`).  This is the ONE site every inter
        // prediction reaches on its way into a plane -- after the compound
        // blend and after the inter-intra blend, before the residual -- so it
        // measures exactly the quantity the oracle's rung measures.  Pairing
        // is by (plane, x, y, w, h): the same chroma geometry recurs in many
        // pictures, so a rung line alone is ambiguous, but a geometry whose
        // samples differ by at most 1 is a unique match (any wrong pairing
        // differs by tens).
        // Env: EC_CPRED="x:y" (plane-absolute origin; -1 for either = any).
        if let Ok(spec) = crate::envflags::var("EC_CPRED") {
            let (fx, fy): (i64, i64) = spec
                .split_once(':')
                .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
                .unwrap_or((-1, -1));
            let hit = (fx < 0 || fx == x as i64) && (fy < 0 || fy == y as i64);
            if hit {
                // `PREFILT_PICTURE_IDX` is bumped once per frame just AFTER
                // that frame's last block, so it still reads this frame's own
                // decode-order index here -- the same numbering the oracle's
                // `aom_ec_pict_idx` and the `.fN` prefilter dumps use.
                let pict = PREFILT_PICTURE_IDX.load(std::sync::atomic::Ordering::SeqCst);
                eprintln!(
                    "OUR_CPRED pict={pict} plane={plane} x={x} y={y} w={w} h={h} side={side}"
                );
                for row in 0..h {
                    let src = &prediction[row * side..row * side + w];
                    let mut line = String::with_capacity(w * 4);
                    for v in src {
                        line.push(' ');
                        line.push_str(&v.to_string());
                    }
                    eprintln!("OUR_CPROW{row}:{line}");
                }
            }
        }
        // lane-recoparse: an all-zero residual (60% of the units) is a plain
        // prediction copy -- the same arithmetic with `r == 0`, without
        // reading `side * h` zeros out of [`ZERO_RESIDUAL`].
        if residual.is_empty() {
            for row in 0..h {
                let src = row * side;
                let dst = (y + row) * self.width + x;
                let pred = &prediction[src..src + w];
                let out = &mut self.data.to_mut()[dst..dst + w];
                for (col, (o, &p)) in out.iter_mut().zip(pred).enumerate() {
                    let v = i32::from(p).clamp(0, max) as u16;
                    pix_write("mc_copy", x, y, w, x + col, y + row, *o, v);
                    *o = v;
                }
            }
            return;
        }
        let residual = dense_residual(residual, side * h);
        for row in 0..h {
            let src = row * side;
            let dst = (y + row) * self.width + x;
            let pred = &prediction[src..src + w];
            let res = &residual[src..src + w];
            let out = &mut self.data.to_mut()[dst..dst + w];
            for (col, ((o, &p), &r)) in out.iter_mut().zip(pred).zip(res).enumerate() {
                let v = (i32::from(p) + r).clamp(0, max) as u16;
                pix_write("mc_add", x, y, w, x + col, y + row, *o, v);
                *o = v;
            }
        }
    }
}

/// Reads one inter-coded plane's transform block and reconstructs it by
/// adding its residual onto a motion-compensated `prediction` -- the inter
/// counterpart of [`read_plane`] (every inter transform this decoder reads is
/// `side`-square with no oversized-64 case, so there is no padded-grid
/// branch to mirror).
#[allow(clippy::too_many_arguments)]
pub(super) fn read_inter_plane(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    set: TxbSet,
    scan: &[u16],
    plane_idx: usize,
    around: (bool, bool, i32),
    tx_mode: usize,
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    x: usize,
    cy: usize,
    side: usize,
    _base_q_idx: u8,
    prediction: Pred<'_>,
    // lane-chromau: this block's own luma transform's *actual coded*
    // `tx_type` -- `av1_get_tx_type` (libaom `blockd.h`): an inter block's
    // chroma plane never codes its own `tx_type` symbol, but (unlike an
    // intra block, which falls back to `Intra_Mode_To_Tx_Type`) it inherits
    // the colocated luma transform's coded type, narrowed to the extended
    // set the CHROMA size selects ([`reduce_inherited_chroma_tx_type`]).
    // `None` for the luma call itself
    // (`plane_idx == 0`), where the `TxbSet`'s own symbol (or the true
    // `DCT_DCT` default at 32-point+) already resolves it without help.
    inherited_luma_tx_type: Option<TxType>,
    // lane-txselect: the luma `txb_skip_ctx` of a transform unit smaller than
    // its own block (spec `get_txb_ctx_general`'s `plane_bsize != tx_size`
    // branch, [`Neighbours::luma_skip_ctx`]) -- `None` for the whole-block
    // transform every non-var-tx caller reads, whose plane-0 context is the
    // lone-transform-unit 0.
    luma_skip_ctx: Option<usize>,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, TxType)> {
    // lane-av1mono: as [`read_plane`] -- a monochrome frame codes no chroma
    // coefficients (`plane < av1_num_planes == 1`).
    if plane_idx > 0 && mono(fctx) {
        return Ok((Grid::Zero(side * side), TxType::DctDct));
    }
    // lane-av1readcensus: the units read from here on are this plane's.
    set_census_plane(plane_idx);
    let skip_ctx = if plane_idx == 0 {
        luma_skip_ctx.unwrap_or(0)
    } else {
        // lane-sb128c r1: on a CHROMA plane this same parameter carries
        // `get_txb_ctx`'s own offset instead -- `combine_entropy_contexts(..)`
        // is +7 normally and +10 when the plane block is LARGER than the
        // transform (a 128x128 block's 64x64 uv plane block with TX_32X32
        // units, AV1's only such case), and the `txb_skip_chroma_32` table
        // carries the offset-10 rows at +3 (lane-sb128b r3).
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
    let default_tx_type = match inherited_luma_tx_type {
        Some(t) => reduce_inherited_chroma_tx_type(t, side, side, fctx),
        None => TxType::DctDct,
    };
    // libaom `av1_get_tx_type` (`blockd.h`): a lossless block's type is
    // DCT_DCT on EVERY plane, so chroma never inherits or mode-derives one.
    let default_tx_type = if lossless(fctx) {
        TxType::DctDct
    } else {
        default_tx_type
    };
    let (grid, tx_type) = read_coeffs(
        dec,
        &mut coding,
        scan,
        skip_ctx,
        dc_sign_ctx(around.2),
        default_tx_type,
        None,
    )?;
    note_chroma_class1(plane_idx, tx_type);
    if crate::envflags::env_flag!("EC_TRACE_TXTYPE") {
        eprintln!(
            "OUR_TXTYPE plane={plane_idx} x={x} y={cy} side={side} tx_type={tx_type:?} inh={inherited_luma_tx_type:?}"
        );
    }
    // lane-inter8 r1: a 64-point transform covers the whole 64x64 area but
    // codes only its top-left 32x32 of frequencies (spec 5.11.40) -- the
    // caller hands the 32-point `scan` there, and the coded corner goes into
    // the top-left of the true `side`-sized grid before dequantization
    // (dqDenom is keyed by the true size, not the corner's), mirroring
    // [`read_plane`]'s own `tx_side != side` branch. Every other transform
    // this decoder codes has `tx_side == side`, so this is inert below 64.
    let tx_side = side.min(32);
    let grid = extend_corner(grid, tx_side, tx_side, side, side);
    let (dc_delta, ac_delta) = plane_q_delta(plane_idx, fctx);
    let tx = TxParams {
        plane: plane_idx,
        w: side,
        h: side,
        bit_depth: bit_depth(fctx),
        q_idx: block_q_idx(fctx),
        dc_delta,
        ac_delta,
        tx_type,
        qm: block_iqmatrix(fctx, plane_idx, side, side, tx_type),
        stride: 0,
        lossless: lossless(fctx),
    };
    push_mc_rect_tx(
        y, u, v, plane_idx, x, cy, side, side, side, prediction, tx, &grid, fctx,
    );
    Ok((grid, tx_type))
}

/// The intra twin of [`read_inter_chroma_lossless`]: an INTRA block inside a
/// lossless frame codes its chroma plane as 4x4 transform units, plane-major
/// inside each 64x64 mu chunk (libaom `decode_token_recon_block`'s
/// `for (plane..) for (blk_row..) for (blk_col..)` with `av1_get_tx_size`
/// returning TX_4X4 under `xd->lossless[..]`). CfL never rides here:
/// `is_cfl_allowed` (`blockd.h`) needs a BLOCK_4X4 chroma plane block, i.e. a
/// luma block of 8x8 or less, which the sub-8x8 readers own.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_intra_chroma_lossless(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    at_mi: (usize, usize),
    (px, py): (usize, usize),
    (cpx, cpy): (usize, usize),
    side: usize,
    (blk_w, blk_h): (usize, usize),
    (org_x, org_y): (usize, usize),
    (reg_w, reg_h): (usize, usize),
    stride: usize,
    reach: Reach,
    (frame_w, frame_h): (usize, usize),
    mode: usize,
    uv_predict_mode: usize,
    angle_delta_uv: i32,
    palette_uv_bufs: Option<(&[u16], &[u16])>,
    smooth_neighbor_uv: bool,
    base_q_idx: u8,
    u_out: &mut Vec<i32>,
    v_out: &mut Vec<i32>,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let scan4 = default_scan(4);
    let offset = (blk_w > 4 || blk_h > 4).then_some(3);
    for plane_idx in 1..3 {
        for rr in 0..reg_h / 4 {
            for cc in 0..reg_w / 4 {
                let (ox, oy) = (org_x + cc * 4, org_y + rr * 4);
                // libaom's `max_blocks_wide/high` clip: a unit whose origin is
                // past the frame edge is not coded at all
                // (`decode_token_recon_block`), same test `decode_block`'s
                // luma loop already runs.
                if cpx + ox >= u.true_width || cpy + oy >= u.true_height {
                    continue;
                }
                // Chroma pixels per mi cell: 2 at 4:2:0, 4 at 4:4:4. At
                // 4:2:0 a TX_4X4 chroma unit spans two mi cells and anchors
                // at the even one; at 4:4:4 it IS one mi cell.
                let cu_mi = (
                    at_mi.0 + oy / (MI >> ss_y(fctx)),
                    at_mi.1 + ox / (MI >> ss_x(fctx)),
                );
                // libaom's chroma entropy-context arrays are SUBSAMPLED:
                // `get_txb_ctx` reads exactly one entry per plane for a
                // TX_4X4 chroma unit (`a[0]`/`l[0]`). Our bands are luma-mi
                // indexed and carry that one entry replicated over the unit's
                // two cells, so one cell is the same answer -- and the second
                // cell is past the band on a unit whose block hangs off the
                // frame edge, which libaom still codes.
                let cu_around = neighbours.around_mi(cu_mi, MI);
                // The unit's origin and extent in LUMA pixels: `<< ss` at
                // 4:2:0 (8x8 luma for a 4x4 chroma unit), identity at 4:4:4.
                let cu_reach = tu_reach_rect(
                    side,
                    side,
                    ox << ss_x(fctx),
                    oy << ss_y(fctx),
                    4 << ss_x(fctx),
                    4 << ss_y(fctx),
                    reach,
                    px,
                    py,
                    frame_w,
                    frame_h,
                    fctx,
                );
                if let Some((ub, vb)) = palette_uv_bufs {
                    let buf = if plane_idx == 1 { ub } else { vb };
                    set_palette_pred(palette_window(buf, stride, ox, oy, 4, 4), fctx);
                }
                let cu_grid = read_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    plane_idx,
                    cu_around[plane_idx],
                    mode,
                    uv_predict_mode,
                    angle_delta_uv,
                    cu_reach,
                    y,
                    u,
                    v,
                    cpx + ox,
                    cpy + oy,
                    4,
                    4,
                    base_q_idx,
                    None,
                    None,
                    offset,
                    smooth_neighbor_uv,
                    fctx,
                )?;
                // The unit's own footprint on the LUMA mi grid: 8 luma px
                // (two cells) at 4:2:0, 4 (one cell) at 4:4:4.
                neighbours.record_mi_chroma(
                    cu_mi,
                    4 << ss_x(fctx),
                    4 << ss_y(fctx),
                    plane_idx,
                    &cu_grid,
                );
                hit!(CHROMA_SPLIT_TX_HITS);
                let dst = mu_units(
                    if plane_idx == 1 {
                        &mut *u_out
                    } else {
                        &mut *v_out
                    },
                    // lane-av1422lpf site 7: `stride * blk_h`, the intra twin
                    // of site 1. `read_intra_chroma_lossless`'s own
                    // `(blk_w, blk_h)` parameter is the block's whole CHROMA
                    // PLANE BLOCK, per-axis, and the walk below addresses up
                    // to row `org_y + reg_h - 1` at `stride` -- so the
                    // requirement is `stride * blk_h` and `stride * stride`
                    // underruns by exactly the ratio the plane block's height
                    // exceeds its width. Identical number at 4:2:0
                    // (`blk_h == stride`) and 4:4:4.
                    stride * blk_h,
                );
                for row in 0..4 {
                    let start = (oy + row) * stride + ox;
                    dst[start..start + 4].copy_from_slice(&cu_grid[row * 4..][..4]);
                }
            }
        }
    }
    Ok(())
}

/// lane-av1h5: the CHUNK-MAJOR token order for a block's var-tx leaf list.
///
/// libaom's `decode_token_recon_block` (`av1/decoder/decodeframe.c:972`)
/// iterates 64x64 "mu" chunks with the PLANE loop INSIDE the chunk loop:
/// `for (row..) for (col..) { for (plane..) for (blk_row..) for (blk_col..) }`,
/// `blk_row`/`blk_col` restarting at the chunk origin, on a chunk grid of
/// `mi_size_wide[BLOCK_64X64]` = 16 mi per axis.
///
/// The transform tree hands us a BLOCK-wide raster instead (32 leaves across
/// at 128), whose chunk members are NOT contiguous: leaves 0..15 are chunk
/// (0,0) row 0, but chunk (0,0) rows 1..15 are leaves 32..47, 64..79, ...
/// Walking it as given closes each chunk every SIXTEEN leaves and codes its
/// chroma sixteen times -- 64 chroma walks per 128 root where libaom codes 4,
/// and 24576 chroma units per inter frame against the oracle's 1536.
///
/// Below 64 a block IS one chunk, so the two orders coincide and the sort is
/// skipped (the returned order is the identity). `changed` is `true` only
/// when the sort actually permutes the list, so a no-op ordering reads zero
/// on [`MU_CHUNK_ORDER_HITS`] and cannot make a gate look armed.
pub(super) fn mu_chunk_order(
    leaves: &[(usize, usize, usize, usize)],
    side: usize,
) -> (Vec<usize>, bool) {
    let identity: Vec<usize> = (0..leaves.len()).collect();
    if side <= 64 {
        return (identity, false);
    }
    let nchunk = (side / 64).max(1);
    let mut order = identity.clone();
    order.sort_by_key(|&i| ((leaves[i].0 / 16) * nchunk + (leaves[i].1 / 16), i));
    let changed = order != identity;
    if changed {
        hit!(MU_CHUNK_ORDER_HITS);
    }
    (order, changed)
}

/// lane-lossless2: the chroma half of a lossless INTER block. libaom's
/// `read_tx_size` (`av1/decoder/decodeframe.c`) returns TX_4X4 for EVERY
/// plane of a lossless block, so the chroma plane block is a raster of 4x4
/// transform units instead of one whole-block transform, and
/// `decode_token_recon_block` walks them PLANE-MAJOR inside each 64x64 "mu"
/// chunk (`for (plane..) for (blk_row..) for (blk_col..)`): every U unit of
/// the chunk, then every V unit. Reads one chunk-sized region
/// (`org`/`reg`, chroma pixels relative to the block) and copies its units
/// into the block-sized `u_out`/`v_out` grids.
#[allow(clippy::too_many_arguments)]
pub(super) fn read_inter_chroma_lossless(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    su: Pred<'_>,
    sv: Pred<'_>,
    at_mi: (usize, usize),
    (cpx, cpy): (usize, usize),
    (org_x, org_y): (usize, usize),
    (reg_w, reg_h): (usize, usize),
    // the whole block's chroma plane block -- what `get_txb_ctx`'s offset is
    // decided on.
    (blk_w, blk_h): (usize, usize),
    // the stride the predictors and the block-sized `u_out`/`v_out` are read
    // at (the block's square chroma side, even for a 2:1 strip, which embeds
    // its narrower plane block in the same square grid).
    stride: usize,
    mode_for_tx: usize,
    base_q_idx: u8,
    u_out: &mut Vec<i32>,
    v_out: &mut Vec<i32>,
    fctx: &crate::decode::FrameCtx,
) -> Result<()> {
    let scan4 = default_scan(4);
    // `get_txb_ctx` (`av1/common/txb_common.h`): `ctx_offset` is 10 when the
    // plane block is bigger than the transform and 7 when it is the
    // transform -- our chroma tables carry the offset-10 rows at +3.
    let offset = (blk_w > 4 || blk_h > 4).then_some(3);
    // lane-av1h5: a plane block above 64 is the 128-root class, whose chroma
    // libaom walks one 64x64 mu chunk at a time. Counted so a gate can pin
    // the walk/unit totals exactly rather than as a floor on wrong output.
    let counted_128 = blk_w > 64 || blk_h > 64;
    if counted_128 {
        hit!(MU_CHUNK_WALKS);
    }
    for plane_idx in 1..3 {
        for rr in 0..reg_h / 4 {
            for cc in 0..reg_w / 4 {
                let (ox, oy) = (org_x + cc * 4, org_y + rr * 4);
                // libaom's `max_blocks_wide/high` clip: a unit whose origin is
                // past the frame edge is not coded at all
                // (`decode_token_recon_block`), same test `decode_block`'s
                // luma loop already runs.
                if cpx + ox >= u.true_width || cpy + oy >= u.true_height {
                    continue;
                }
                // The chroma neighbour bands are indexed in LUMA mi: a 4-wide
                // chroma unit spans `4 << ss` luma pixels, i.e. `4 >> ss` --
                // 2 mi cells at 4:2:0, one at 4:4:4.
                let cu_mi = (
                    at_mi.0 + oy / (4 >> ss_y(fctx)),
                    at_mi.1 + ox / (4 >> ss_x(fctx)),
                );
                // libaom's chroma entropy-context arrays are SUBSAMPLED:
                // `get_txb_ctx` reads exactly one entry per plane for a
                // TX_4X4 chroma unit (`a[0]`/`l[0]`). Our bands are luma-mi
                // indexed and carry that one entry replicated over the unit's
                // two cells, so one cell is the same answer -- and the second
                // cell is past the band on a unit whose block hangs off the
                // frame edge, which libaom still codes.
                let cu_around = neighbours.around_mi(cu_mi, MI);
                let src = if plane_idx == 1 { su } else { sv };
                let cu_grid = read_inter_plane(
                    dec,
                    cdfs,
                    TxbSet::Chroma4,
                    scan4,
                    plane_idx,
                    cu_around[plane_idx],
                    mode_for_tx,
                    y,
                    u,
                    v,
                    cpx + ox,
                    cpy + oy,
                    4,
                    base_q_idx,
                    src.offset(oy * stride + ox),
                    // A lossless block codes DCT_DCT on every plane
                    // (`av1_get_tx_type`, `blockd.h`), so nothing is
                    // inherited from the luma unit.
                    None,
                    offset,
                    fctx,
                )?
                .0;
                // Immediately, so the NEXT unit reads this one's context.
                neighbours.record_mi_chroma(
                    cu_mi,
                    4 << ss_x(fctx),
                    4 << ss_y(fctx),
                    plane_idx,
                    &cu_grid,
                );
                hit!(CHROMA_SPLIT_TX_HITS);
                if counted_128 {
                    hit!(MU_CHUNK_WALK_UNITS);
                }
                let dst = mu_units(
                    if plane_idx == 1 {
                        &mut *u_out
                    } else {
                        &mut *v_out
                    },
                    // lane-av1422lpf site 1: `stride * blk_h`, NOT
                    // `stride * stride`. The composed `u_out`/`v_out` grid is
                    // the block's whole CHROMA PLANE BLOCK read at `stride`,
                    // and libaom's plane block is PER-AXIS
                    // (`(bw >> ss_x) x (bh >> ss_y)`, decodeframe.c's
                    // `dec->uv_buffer` extent) -- square only when
                    // `ss_x == ss_y`. The walk above addresses row
                    // `org_y + reg_h - 1` at column `org_x + reg_w`, and every
                    // call site passes a region that is a sub-rect of `blk`
                    // with `org` inside `blk`, so the requirement is exactly
                    // `stride * blk_h` (the last row's end
                    // `(blk_h - 1) * stride + blk_w` fits because
                    // `blk_w <= stride` at all nine call sites).
                    //
                    // Where the plane block IS square -- 4:2:0 (`side/2` both
                    // axes) and 4:4:4 (`side` both axes) on a square block --
                    // `blk_h == stride` and this reduces to the old
                    // `stride * stride` verbatim, so it cannot move a byte
                    // there. It is not 4:2:2-only either: a 64x128 4:2:0 block
                    // has a 32x64 chroma plane block, and `ll420_a.obu`
                    // (`range end index 1028 out of range for slice of
                    // length 1024`) panicked on the old expression.
                    stride * blk_h,
                );
                for row in 0..4 {
                    let start = (oy + row) * stride + ox;
                    dst[start..start + 4].copy_from_slice(&cu_grid[row * 4..][..4]);
                }
            }
        }
    }
    Ok(())
}

/// The 8x8 inter leaf's lossless chroma arm (lane-av1-llinter), shared by the
/// single-ref and compound paths, for every plane block BIGGER than 4x4:
/// libaom's `read_tx_size` (`av1/decoder/decodeframe.c:1197`,
/// `if (xd->lossless[xd->mi[0]->segment_id]) return TX_4X4;`) returns TX_4X4
/// for EVERY plane of a lossless block, not just luma, so the plane block is
/// a raster of TX_4X4 units read by the same per-unit walk the bigger inter
/// blocks run.
///
/// **The plane block is PER-AXIS**, `av1_ss_size_lookup[BLOCK_8X8][ss_x][ss_y]`
/// (`av1/common/blockd.h:1186`): BLOCK_8X8 at 4:4:4 (2x2 units), BLOCK_4X8 at
/// 4:2:2 (1x2 units), BLOCK_4X4 at 4:2:0 (one unit, `get_txb_ctx`'s
/// `plane_bsize == txsize_to_bsize[tx_size]` branch, ctx_offset 7 -- the one
/// shape that keeps the single-unit read and is NOT routed here). Gating this
/// arm on the 4:4:4 shape instead left 4:2:2 coding its 4x8 plane block as ONE
/// TX_4X8 unit at ctx_offset 7, where libaom reads TWO TX_4X4 units at
/// ctx_offset 10 (`av1/common/txb_common.h:430-433`): the first unit's
/// `txb_skip` symbol then came out of the wrong CDF row, which desynchronised
/// the coder for the rest of the frame.
///
/// `(blk_w, blk_h)` is that per-axis plane block and `stride` the composed
/// grid's row pitch (the leaf's `chroma_w`); the grids are `stride * blk_h`
/// (`Grid::Zero` when every unit hung off the frame edge) and each unit's own
/// coefficient context is left stamped in the neighbour bands -- the caller's
/// tail must not let its whole-block `record_mi` overwrite them.
#[allow(clippy::too_many_arguments)]
pub(super) fn leaf8_inter_chroma_lossless(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    neighbours: &mut Neighbours,
    su: Pred<'_>,
    sv: Pred<'_>,
    at_mi: (usize, usize),
    (cpx, cpy): (usize, usize),
    // The leaf's CHROMA PLANE BLOCK, per axis, and the composed grid's row
    // pitch -- `(8, 8)`/`8` at 4:4:4, `(4, 8)`/`4` at 4:2:2.
    (blk_w, blk_h): (usize, usize),
    stride: usize,
    base_q_idx: u8,
    fctx: &crate::decode::FrameCtx,
) -> Result<(Grid, Grid)> {
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
        (blk_w, blk_h),
        (blk_w, blk_h),
        stride,
        0,
        base_q_idx,
        &mut uo,
        &mut vo,
        fctx,
    )?;
    let n = stride * blk_h;
    Ok((
        if uo.is_empty() {
            Grid::Zero(n)
        } else {
            Grid::Own(uo)
        },
        if vo.is_empty() {
            Grid::Zero(n)
        } else {
            Grid::Own(vo)
        },
    ))
}

/// Spec 5.11.25's `single_ref_p1`..`p6` tree (reachable only once `comp_mode`
/// is known `SINGLE_REFERENCE` -- `decode_stream` refuses any frame with
/// `reference_select` set before this is ever called, so that read is never
/// needed here). `above_ref`/`left_ref` are `-1` for an intra/unavailable
/// neighbour, else the `1..=7` reference that neighbour coded (lane-av1refs).
pub(super) fn read_single_ref(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above_ref: i8,
    above_ref1: Option<i8>,
    left_ref: i8,
    left_ref1: Option<i8>,
) -> i8 {
    let above = (above_ref > 0).then_some(above_ref);
    let left = (left_ref > 0).then_some(left_ref);
    let p1 =
        dec.symbol(&mut cdfs.single_ref[single_ref_p1_ctx(above, above_ref1, left, left_ref1)][0]);
    let ref_frame = if p1 == 1 {
        let p2 = dec
            .symbol(&mut cdfs.single_ref[single_ref_p2_ctx(above, above_ref1, left, left_ref1)][1]);
        if p2 == 1 {
            ALTREF_FRAME
        } else {
            let p6 = dec.symbol(
                &mut cdfs.single_ref[single_ref_p6_ctx(above, above_ref1, left, left_ref1)][5],
            );
            if p6 == 1 { ALTREF2_FRAME } else { BWDREF_FRAME }
        }
    } else {
        let p3 = dec
            .symbol(&mut cdfs.single_ref[single_ref_p3_ctx(above, above_ref1, left, left_ref1)][2]);
        if p3 == 1 {
            let p5 = dec.symbol(
                &mut cdfs.single_ref[single_ref_p5_ctx(above, above_ref1, left, left_ref1)][4],
            );
            if p5 == 1 { GOLDEN_FRAME } else { LAST3_FRAME }
        } else {
            let p4 = dec.symbol(
                &mut cdfs.single_ref[single_ref_p4_ctx(above, above_ref1, left, left_ref1)][3],
            );
            if p4 == 1 { LAST2_FRAME } else { LAST_FRAME }
        }
    };
    if ref_frame != LAST_FRAME {
        hit!(NON_LAST_REF_HITS);
        let c = match ref_frame {
            LAST2_FRAME => &LAST2_HITS,
            LAST3_FRAME => &LAST3_HITS,
            GOLDEN_FRAME => &GOLDEN_HITS,
            BWDREF_FRAME => &BWDREF_HITS,
            ALTREF2_FRAME => &ALTREF2_HITS,
            ALTREF_FRAME => &ALTREF_HITS,
            _ => unreachable!("read_single_ref only ever returns LAST_FRAME..=ALTREF_FRAME"),
        };
        hit_do! { c.with(|v| v.set(v.get() + 1)); }
    }
    ref_frame
}

/// Spec 5.11.25's `comp_mode` (lane-av1comp): whether an inter block reads
/// `SINGLE_REFERENCE` or `COMPOUND_REFERENCE`, only reached once a frame's
/// `reference_select` header bit lets a block choose per-block instead of
/// fixing the mode. `above`/`left` are `None` for an intra/unavailable
/// neighbour, [`NeighbourRef`] for a coded one -- [`reference_mode_ctx`]'s
/// own doc. Called from [`decode_inter_block`] once its own `reference_select`
/// parameter is set; [`decode_inter_block8`]'s 8x8 leaf path gates the same
/// way with a coarser (`LAST_FRAME`-only) neighbour shape.
/// lane-twostage: this frame's references as DIMENSIONS now and PIXELS on
/// demand. An inter tile's parse reads reference SIZES (`mc::scale_factor`)
/// only, so the pictures are resolved at the first [`ref_planes`] call
/// rather than at frame entry. On the threaded path that resolution is a
/// WAIT on the producing frame's pixels, which is what lets a consumer parse
/// while its references are still being filtered.
/// lane-defer8: the prediction that asks for them can run in a closure, see
/// [`ref_planes_deferred`].
pub(crate) struct RefPix<'p> {
    /// `(width, height)` per `ref_frame` index (`LAST_FRAME` = 1). `None` is
    /// a slot this frame may not use: empty. A reference of another size is
    /// predicted from through `mc::RefScale` (spec 7.11.3.3).
    pub(super) dims: [Option<(usize, usize)>; 8],
    pub(super) src: RefSrc<'p>,
    pub(super) bufs: std::sync::OnceLock<[Option<(PlaneBuf<'p>, PlaneBuf<'p>, PlaneBuf<'p>)>; 8]>,
    /// lane-defer8: a prediction's [`ref_planes`] failure, parked here for
    /// the frame decoder to take ([`take_err`](Self::take_err)).
    pub(super) err: std::sync::Mutex<Option<Error>>,
    /// The same references as shared pictures, when they can be handed to a
    /// recon worker (the threaded stream): the worker waits for them itself.
    pub(super) shared: Option<SharedRefs>,
}

/// A frame's reference pictures as `Arc`s behind a wait, for the recon
/// workers that build predictions off the parse thread.
#[derive(Clone)]
pub(crate) struct SharedRefs {
    pub(crate) dims: [Option<(usize, usize)>; 8],
    pub(crate) wait: std::sync::Arc<
        dyn Fn() -> Result<[Option<std::sync::Arc<Picture>>; 8]> + Send + Sync,
    >,
}

/// Where a [`RefPix`] gets its pictures: already in hand, or behind a wait
/// the caller owns (`stream.rs`'s reference promises).
pub(super) enum RefSrc<'p> {
    Ready([Option<&'p Picture>; 8]),
    Lazy(&'p (dyn Fn() -> Result<[Option<&'p Picture>; 8]> + Send + Sync + 'p)),
}

/// spec 7.11.3.3's scale of a reference of `(width, height)` (cropped luma
/// sizes) against this frame's `(width, height)`.
pub(crate) fn scale_of(
    (ref_width, ref_height): (usize, usize),
    (frame_width, frame_height): (usize, usize),
) -> mc::RefScale {
    mc::RefScale::new(ref_width, ref_height, frame_width, frame_height)
}

impl<'p> RefPix<'p> {
    /// References already decoded: the single-threaded path and the test
    /// entry points, where [`ref_planes`] never waits.
    pub(crate) fn ready(refs: [Option<&'p Picture>; 8]) -> Self {
        let dims = std::array::from_fn(|i| refs[i].map(|p| (p.width, p.height)));
        Self {
            dims,
            src: RefSrc::Ready(refs),
            bufs: std::sync::OnceLock::new(),
            err: std::sync::Mutex::new(None),
            shared: None,
        }
    }

    /// References whose sizes are known (from their producers' headers) but
    /// whose pixels `wait` still has to produce.
    pub(crate) fn lazy(
        dims: [Option<(usize, usize)>; 8],
        wait: &'p (dyn Fn() -> Result<[Option<&'p Picture>; 8]> + Send + Sync + 'p),
        shared: Option<SharedRefs>,
    ) -> Self {
        Self {
            dims,
            src: RefSrc::Lazy(wait),
            bufs: std::sync::OnceLock::new(),
            err: std::sync::Mutex::new(None),
            shared,
        }
    }

    /// Whether a recon worker can wait for these references itself.
    pub(super) fn is_shared(&self) -> bool {
        self.shared.is_some()
    }

    /// The shareable form of these references, if there is one.
    pub(super) fn shared(&self) -> Option<SharedRefs> {
        self.shared.clone()
    }

    /// This slot's picture size, without touching its pixels.
    pub(crate) fn dims(&self, ref_frame: i8) -> Option<(usize, usize)> {
        self.dims.get(ref_frame as usize).copied().flatten()
    }

    /// Parks a prediction's failure (first one wins).
    pub(super) fn park_err(&self, e: Error) {
        let mut slot = self
            .err
            .lock()
            .expect("the parked-error slot is never poisoned");
        if slot.is_none() {
            *slot = Some(e);
        }
    }

    /// The first failure any prediction parked, taken once the frame's tiles
    /// are decoded.
    pub(crate) fn take_err(&self) -> Result<()> {
        match self
            .err
            .lock()
            .expect("the parked-error slot is never poisoned")
            .take()
        {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Every usable slot's three planes, resolving (and, on the threaded
    /// path, waiting for) the pictures on the first call.
    pub(super) fn planes(
        &self,
    ) -> Result<&[Option<(PlaneBuf<'p>, PlaneBuf<'p>, PlaneBuf<'p>)>; 8]> {
        if let Some(bufs) = self.bufs.get() {
            return Ok(bufs);
        }
        let pics = match &self.src {
            RefSrc::Ready(refs) => *refs,
            RefSrc::Lazy(wait) => wait()?,
        };
        let dims = self.dims;
        let bufs = std::array::from_fn(|i| {
            dims[i].and(pics[i]).map(|g| {
                (
                    whole_plane(&g.y, g.width, g.height),
                    {
                        // No subsampling field on a stored reference: the
                        // chroma format comes off its own sample count --
                        // 4:4:4 `w * h`, 4:2:2 `round_ss(w, 1) * h`, 4:2:0
                        // `round_ss(w, 1) * round_ss(h, 1)`. The helper's own
                        // doc carries the measurement behind those three arms
                        // and the pre-existing odd-dimension 4:2:0 gap.
                        let (cw, ch) = ref_chroma_shape(g.u.len(), g.width, g.height);
                        whole_plane(&g.u, cw, ch)
                    },
                    {
                        let (cw, ch) = ref_chroma_shape(g.v.len(), g.width, g.height);
                        whole_plane(&g.v, cw, ch)
                    },
                )
            })
        });
        let _ = self.bufs.set(bufs);
        Ok(self.bufs.get().expect("the plane array was just set"))
    }
}

/// This slot's picture SIZE, with [`ref_planes`]' own error when the slot
/// carries no usable picture -- `dims` is `None` exactly when `planes()`
/// would have nothing there, so the parse thread keeps refusing the same
/// streams at the same block WITHOUT waiting for a producer's pixels
/// (lane-defer8).
pub(super) fn ref_dims(ref_frame: i8, refpix: &RefPix<'_>) -> Result<(usize, usize)> {
    refpix.dims(ref_frame).ok_or_else(|| {
        if crate::envflags::env_flag!("EC_AV1_REFPROBE") {
            eprintln!(
                "EC_REFPROBE missing ref {ref_frame}\n{}",
                std::backtrace::Backtrace::force_capture()
            );
        }
        // lane-hdrlossless: libaom errors on the same stream
        // (`decodeframe.c:5027`: `if (cm->ref_frame_map[ref] == NULL)
        // aom_internal_error(... AOM_CODEC_CORRUPT_FRAME, "Inter frame
        // requests nonexistent reference")`) -- a spec-conformant guard.
        unsupported(
            "a reference frame selected with no picture at this frame's own \
                 ref_frame_idx slot for it",
        )
    })
}

/// [`ref_planes`] from inside a prediction closure, where there is no `?`
/// to take: the error is parked on the [`RefPix`] and taken by the frame
/// decoder ([`RefPix::take_err`]). The block's
/// prediction is left zeroed -- the frame is failing either way.
pub(super) fn ref_planes_deferred<'a, 'p>(
    ref_frame: i8,
    refpix: &'a RefPix<'p>,
) -> Option<(&'a PlaneBuf<'p>, &'a PlaneBuf<'p>, &'a PlaneBuf<'p>)> {
    match ref_planes(ref_frame, refpix) {
        Ok(planes) => Some(planes),
        Err(e) => {
            refpix.park_err(e);
            None
        }
    }
}

/// A whole reference plane as a [`PlaneBuf`]: no tile window, no margin.
pub(super) fn whole_plane(data: &[u16], width: usize, height: usize) -> PlaneBuf<'_> {
    PlaneBuf {
        data: std::borrow::Cow::Borrowed(data),
        width,
        height,
        true_width: width,
        true_height: height,
        tile_x0: 0,
        tile_y0: 0,
        tile_x1: width,
        tile_y1: height,
        plane: 0,
    }
}

/// A reference picture's own chroma plane shape, `(width, height)`, from its
/// luma size and its chroma sample count. A stored reference carries no
/// subsampling field, so the format is read back off the count -- and the
/// counts are the producer's own [`round_ss`] crops
/// (`decode.rs:25806`), so the test must use `round_ss` too and not a bare
/// halving: 4:4:4 stores `w * h`, 4:2:2 stores `round_ss(w, 1) * h` (half
/// width CEIL, full height), 4:2:0 stores `round_ss(w, 1) * round_ss(h, 1)`.
/// A floor test here silently misses every odd-width 4:2:2 reference and
/// drops it into the 4:2:0 arm -- the same defect one dimension over.
///
/// The two-way test this replaces ("luma-sized, else a quarter") folded 4:2:2
/// into the 4:2:0 shape entirely, so a 4:2:2 reference plane claimed
/// `h / 2` rows -- half the samples it actually holds. Every chroma MC
/// reading such a plane then had its source row clamped against
/// `true_height` (spec 7.11.3.4 / libaom's
/// `dec_calc_subpel_params_and_extend` clamps to the reference's valid
/// area), which folded every chroma row at or past the frame's midpoint
/// back onto the last valid row: on the pinned t422 fixture frame 1, the
/// 8x4 inter strip at luma (88, 208) MC-predicted chroma (88, 104) from
/// frame 0 with mv 0 and a pure copy, yet wrote (206, 204, 202, 200) x4
/// rows where the reference holds (202, 204, 204, 167) /
/// (185, 190, 190, 165) / ...
/// **Ambiguity, measured rather than argued away:** the inference reads a
/// format off a length, so it is only as good as the lengths being
/// distinct. Brute-forced against the producer's own `round_ss` crops over
/// every `w, h` in `1..=128`, the 4:4:4 and 4:2:2 counts are distinct from
/// each other and from the 4:2:0 count, and both route correctly for
/// **every `w >= 2 && h >= 2`**. No stream this decoder accepts reaches a
/// 1-pixel chroma plane; the pinned fixture's chroma is 128x144.
///
/// **4:2:0 at odd dimensions: CLOSED by lane-av1odd440.** The arm used to be
/// the old `(width / 2, height / 2)` floor, which was what the two-way test
/// this replaces produced -- harmless then, since no 4:2:0 stream changed
/// shape, and wrong for an odd-dimension 4:2:0 reference: the producer
/// stored `round_ss(w, 1) x round_ss(h, 1)`, so the floor arm
/// under-declared. Measured over the `1..=128` square, running each 4:2:0
/// count through this very function: 12160 of the 16384 pairs came back
/// smaller than the plane actually holds, 12033 of those with `w >= 2` and
/// 127 in the `w == 1` column, where the floor cannot even express
/// `(1, round_ss(h, 1))`. It was left alone pending "a lane that can bring
/// odd-dimension 4:2:0 fixtures with it" -- this is that lane: the pinned
/// `420_odd65x65_key.obu` is a real 65x65 4:2:0 key frame (declared at
/// `encode::encode_key_frame_at_size`, because aomenc rounds the coded size
/// down to even and cannot produce one) whose chroma planes are 33x33, and
/// the gate `an_odd_luma_420_key_frame_decodes_pixel_exact` decodes it
/// sample-exact against `aomdec`. The arm is now the producer's own crop,
/// so a 4:2:0 reference at ANY size routes back to the shape the producer
/// wrote. Even sizes are untouched: `(2k + 1) / 2 == k`, so every 4:2:0
/// reference this decoder met before this lane claims exactly the same
/// shape it did then.
pub(super) fn ref_chroma_shape(len: usize, width: usize, height: usize) -> (usize, usize) {
    let half_w = round_ss(width, 1);
    if len == width * height {
        (width, height)
    } else if len == half_w * height {
        (half_w, height)
    } else {
        (round_ss(width, 1), round_ss(height, 1))
    }
}

/// Resolves a reference's own planes -- the `ref_frame -> picture` lookup
/// [`decode_inter_block`]'s single-ref path does, factored out so the
/// compound path can call it twice (lane-av1comp), and the point where
/// lane-twostage's pixel wait happens.
pub(super) fn ref_planes<'a, 'p>(
    ref_frame: i8,
    refpix: &'a RefPix<'p>,
) -> Result<(&'a PlaneBuf<'p>, &'a PlaneBuf<'p>, &'a PlaneBuf<'p>)> {
    match refpix
        .planes()?
        .get(ref_frame as usize)
        .and_then(Option::as_ref)
    {
        Some((y, u, v)) => Ok((y, u, v)),
        None => Err(unsupported(
            "a reference frame selected with no picture at this frame's own \
                 ref_frame_idx slot for it",
        )),
    }
}

/// libaom `foreach_overlappable_nb_above` (`obmc.h`): walks the mi row
/// directly above this block's own column span, one bordering block at a
/// time (stepping by that neighbour's own `MiInfo::size`, which is always
/// `<= 16` since no block this decoder ever codes exceeds 64x64), collecting
/// every *inter* neighbour found (an intra or unset cell is skipped, one mi
/// unit at a time, and does not itself count as a step). `max_neighbors`
/// caller-bounds the scan: `1` for the eligibility check (any hit at all),
/// `av1_build_obmc_inter_prediction`'s own `max_neighbor_obmc` table for the
/// real blend pass. lane-intra16x4 r2: libaom's own "4-wide block, treat as
/// half of a chroma pair" merge (`obmc.h`'s `mi_step == 1` special case) IS
/// ported below -- the round-1 note that no block is narrower than 8px stopped
/// being true when the 1:4 partitions landed. A 16x4 / 4x16 strip has
/// `mi_size_high`/`mi_size_wide` 1, so libaom snaps the walk back to the pair's
/// even index, reads the ODD (chroma-carrying) half as the neighbour and steps
/// by 2. Measured: the 16x16 block right of a 16x16-level HORZ_4 partition
/// whose strip 0 is inter and strips 1..3 intra counted that inter strip as
/// overlappable here, while libaom's pair merge reads the INTRA strip 1 and
/// counts none -- so we read a `motion_mode` symbol libaom never wrote (class
/// `symbol-consumption-gap`), desyncing the tile at the next coefficient.
pub(super) fn overlappable_above(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    mi_cols: usize,
    max_neighbors: usize,
) -> Vec<(usize, usize, MiInfo, usize)> {
    // lane-alloc: bounded by `max_neighbors`, so size it once.
    let mut out = Vec::with_capacity(max_neighbors);
    if mi_row == 0 {
        return out;
    }
    let end_col = (mi_col + bw4).min(mi_cols);
    let mut col = mi_col;
    while col < end_col && out.len() < max_neighbors {
        let cell = grid.get(mi_row - 1, col);
        let mut step = cell
            .map_or(1, |c| c.size as usize)
            .max(1)
            .min(SB_MI as usize);
        let mut nb = cell;
        // lane-obmcrec r1: the mi the neighbour's `MiInfo` is actually taken
        // from -- the pair merge below moves it, and libaom reads that same
        // `mbmi`'s `interp_filters` (`av1_setup_build_prediction_by_above_pred`).
        let mut src = col;
        if step == 1 {
            // The pair merge: the walk snaps back to the pair's even column
            // and the ODD half (the chroma-carrying one) is the neighbour for
            // both -- a 4-wide 1:4 strip is never an OBMC source on its own.
            // lane-av1422warp r33 (reviewer P2): the snap must be UNCONDITIONAL.
            // libaom `obmc.h:41-46` is
            // `above_mi = prev_row_mi + (above_mi_col & ~1) + 1` -- the snapped
            // COLUMN is used as-is and the +1 is what lands it back on the
            // block's own left edge. Clamping the snapped INDEX to `mi_col`
            // (an earlier r25 attempt) moved the index itself, so this
            // decoded the neighbour one column RIGHT of libaom's and, after
            // `col += step`, never visited `mi_col + 1`. Only the REPORTED
            // offset can be negative -- `above_mi - mi_col == -1` -- so the
            // clamp belongs there, not on the index.
            col &= !1;
            src = col + 1;
            nb = grid.get(mi_row - 1, src);
            step = 2;
        }
        if let Some(info) = nb
            && info.is_inter
        {
            if src != col {
                hit!(OBMC_PAIR_FILTER_HITS);
            }
            // lane-t900 r14: libaom passes `AOMMIN(xd->width, mi_step)`
            // as `op_mi_size` -- the block's OWN width, never the
            // frame-edge clamp `end_col` carries. Clamping here shrank the
            // neighbour PREDICTION at a frame edge, which picks the
            // 4-tap kernel below 8 px (class narrow-block-sharp-kernel);
            // the destination clip stays in `obmc_blend`.
            out.push((
                col.saturating_sub(mi_col),
                step.min(bw4),
                info,
                src - mi_col,
            ));
        }
        col += step;
    }
    out
}

/// `foreach_overlappable_nb_left`'s left-column mirror of
/// [`overlappable_above`] -- see its doc for the shared corner-cut.
pub(super) fn overlappable_left(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bh4: usize,
    mi_rows: usize,
    max_neighbors: usize,
) -> Vec<(usize, usize, MiInfo, usize)> {
    // lane-alloc: bounded by `max_neighbors`, so size it once.
    let mut out = Vec::with_capacity(max_neighbors);
    if mi_col == 0 {
        return out;
    }
    let end_row = (mi_row + bh4).min(mi_rows);
    let mut row = mi_row;
    while row < end_row && out.len() < max_neighbors {
        let cell = grid.get(row, mi_col - 1);
        // Vertical walk steps by the neighbour's HEIGHT -- a 32x16 strip's
        // width (8) would swallow the strip below it.
        let mut step = cell
            .map_or(1, |c| c.size_h as usize)
            .max(1)
            .min(SB_MI as usize);
        let mut nb = cell;
        // lane-obmcrec r1: see `overlappable_above`'s `src`.
        let mut src = row;
        if step == 1 {
            // The pair merge, left-column mirror: a 4-TALL neighbour (a 16x4
            // strip) is half of a chroma pair and the pair's SECOND row is the
            // neighbour for both.
            // lane-av1422warp r33 (reviewer P2): the row mirror of the
            // `overlappable_above` fix. libaom's left-column walk snaps
            // `(left_mi_row & ~1)` and adds 1; the snap is unconditional and
            // only the REPORTED `row - mi_row` can be -1. The r25 clamp moved
            // the snapped index itself, which read the neighbour one row
            // BELOW libaom's and then skipped `mi_row + 1` entirely.
            row &= !1;
            src = row + 1;
            nb = grid.get(src, mi_col - 1);
            step = 2;
        }
        if let Some(info) = nb
            && info.is_inter
        {
            if src != row {
                hit!(OBMC_PAIR_FILTER_HITS);
            }
            // lane-t900 r14: `AOMMIN(xd->height, mi_step)` -- see
            // `overlappable_above`.
            out.push((
                row.saturating_sub(mi_row),
                step.min(bh4),
                info,
                src - mi_row,
            ));
        }
        row += step;
    }
    out
}

/// libaom `motion_mode_allowed`'s neighbour term: does ANY bordering inter
/// block sit on this block's above row or left column ("check_num_overlappable
/// _neighbors", `obmc.h`). The single source of truth for the DECODER's own
/// `motion_mode_eligible` and for the tile WRITER's (`tile::write_motion_mode`)
/// -- the two disagreeing by one block writes a `motion_mode` symbol the
/// decoder never reads (class `symbol-consumption-gap`).
pub(crate) fn has_overlappable_neighbour(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
) -> bool {
    !overlappable_above(grid, mi_row, mi_col, bw4, mi_cols, 1).is_empty()
        || !overlappable_left(grid, mi_row, mi_col, bh4, mi_rows, 1).is_empty()
}

/// The warp-sample gather's own `has_top_right` probe (libaom `av1_findSamples`
/// calls the same `mvref_common.c` function the MV stack does). lane-sb128 r2:
/// this used to hardcode `SB_MI` (a 64x64 superblock) -- at `sb_size == 128`
/// libaom's mask loop runs one level further and strips the top-right sample
/// off blocks whose neighbour IS decoded, so a warped 128-superblock frame
/// picked up wrong warp parameters (frame 6 of the 128 inter gate). Delegates
/// to the single sb-aware port in [`crate::mvstack::has_top_right`]; `bs` is
/// the block's own side in 4x4 (`mi`) units (square inter leaves only here).
pub(super) fn has_top_right(
    mi_row: usize,
    mi_col: usize,
    bs: usize,
    fctx: &crate::decode::FrameCtx,
) -> bool {
    crate::mvstack::has_top_right(mi_row, mi_col, bs, bs, fctx)
}

/// `av1_findSamples` (`mvref_common.c`), full port: the up to
/// `LEAST_SQUARES_SAMPLES_MAX` (8) above/left/top-left/top-right neighbour
/// samples (`record_samples`) that share this block's own single reference
/// frame and are themselves single-ref -- `mbmi->num_proj_ref` is
/// `.len()` of the returned vec. `bw4` is the block's own side in 4x4 units
/// (square, so also `bh4`; a real block's `xd->width`/`xd->height` in
/// libaom are also mi units of that same square side here). Each sample's
/// own `bw`/`bh` (fed to [`crate::warp::record_sample`]'s offset formula)
/// come from the *neighbour* cell's own coded size, not this block's.
#[allow(clippy::too_many_arguments)]
pub(crate) fn find_samples(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
    ref_frame: i8,
    fctx: &crate::decode::FrameCtx,
) -> Vec<crate::warp::Sample> {
    const MAX: usize = crate::warp::LEAST_SQUARES_SAMPLES_MAX;
    let up_available = mi_row > 0;
    let left_available = mi_col > 0;
    let mut samples = Vec::with_capacity(MAX);
    // lane-t900 r9: `EC_PROJ_MI=<mi_row>,<mi_col>` lists this block's warp
    // sample candidates in libaom's own order (above, left, top-left,
    // top-right) -- `num_proj_ref >= 1` is what picks the 3-symbol
    // motion_mode alphabet over the 2-symbol obmc one.
    // lane-alloc: the `format!` (and the map lookup's `String` clone) ran on
    // every warp block; the flag itself is a process constant.
    let proj_dbg = crate::envflags::env_flag!("EC_PROJ_MI")
        && crate::envflags::var("EC_PROJ_MI").is_ok_and(|v| v == format!("{mi_row},{mi_col}"));
    let single_ref_match = |info: &MiInfo| {
        let m = info.ref_frame == ref_frame && info.ref_frame1 == NO_REF1;
        if proj_dbg {
            eprintln!(
                "EC_PROJ cand ref={} ref1={:?} size={} size_h={} is_inter={} want={ref_frame} match={m}",
                info.ref_frame, info.ref_frame1, info.size, info.size_h, info.is_inter
            );
        }
        m
    };
    let mut do_tl = true;
    let mut do_tr = true;
    let rec = |info: &MiInfo, row_offset: i32, sign_r: i32, col_offset: i32, sign_c: i32| {
        let nb_bw = (info.size * 4) as i32;
        // lane-rect r2: the neighbour's own true height -- a rect strip
        // neighbour donates its sample at ITS center (aom pts y=-72 for a
        // 32x16 above strip; the square assumption put it at -136).
        let nb_bh = (info.size_h * 4) as i32;
        crate::warp::record_sample(
            nb_bw,
            nb_bh,
            mv32(info.mv),
            row_offset,
            sign_r,
            col_offset,
            sign_c,
        )
    };

    'outer: {
        if up_available {
            let above = grid.get(mi_row - 1, mi_col);
            let superblock_width = above.map_or(1, |c| c.size as usize).max(1);
            if bw4 <= superblock_width {
                let col_offset = -((mi_col % superblock_width) as isize);
                if col_offset < 0 {
                    do_tl = false;
                }
                if col_offset + superblock_width as isize > bw4 as isize {
                    do_tr = false;
                }
                if let Some(info) = above
                    && single_ref_match(&info)
                {
                    samples.push(rec(&info, 0, -1, col_offset as i32, 1));
                    if samples.len() >= MAX {
                        break 'outer;
                    }
                }
            } else {
                let mut i = 0usize;
                let limit = bw4.min(mi_cols.saturating_sub(mi_col));
                while i < limit {
                    let cell = grid.get(mi_row - 1, mi_col + i);
                    let sw = cell.map_or(1, |c| c.size as usize).max(1);
                    if let Some(info) = cell
                        && single_ref_match(&info)
                    {
                        samples.push(rec(&info, 0, -1, i as i32, 1));
                        if samples.len() >= MAX {
                            break 'outer;
                        }
                    }
                    i += sw;
                }
            }
        }

        if left_available {
            let left = grid.get(mi_row, mi_col - 1);
            let superblock_height = left.map_or(1, |c| c.size_h as usize).max(1);
            if bh4 <= superblock_height {
                let row_offset = -((mi_row % superblock_height) as isize);
                if row_offset < 0 {
                    do_tl = false;
                }
                if let Some(info) = left
                    && single_ref_match(&info)
                {
                    samples.push(rec(&info, row_offset as i32, 1, 0, -1));
                    if samples.len() >= MAX {
                        break 'outer;
                    }
                }
            } else {
                let mut i = 0usize;
                let limit = bh4.min(mi_rows.saturating_sub(mi_row));
                while i < limit {
                    let cell = grid.get(mi_row + i, mi_col - 1);
                    let sh = cell.map_or(1, |c| c.size_h as usize).max(1);
                    if let Some(info) = cell
                        && single_ref_match(&info)
                    {
                        samples.push(rec(&info, i as i32, 1, 0, -1));
                        if samples.len() >= MAX {
                            break 'outer;
                        }
                    }
                    i += sh;
                }
            }
        }

        if do_tl
            && left_available
            && up_available
            && let Some(info) = grid.get(mi_row - 1, mi_col - 1)
            && single_ref_match(&info)
        {
            samples.push(rec(&info, 0, -1, 0, -1));
            if samples.len() >= MAX {
                break 'outer;
            }
        }

        // lane-t900 r9: libaom passes `AOMMAX(xd->width, xd->height)` here
        // (`mvref_common.c:1235`), NOT the block's width -- a rect block's
        // top-right reach is probed at its LONGER side. With `bw4` a 64x128
        // block found a top-right sample libaom never sees, so
        // `num_proj_ref >= 1` picked the 3-symbol `motion_mode_cdf` where
        // libaom reads the 2-symbol `obmc_cdf` (class
        // wrong-alphabet-same-value).
        let tr_reach = crate::mvstack::has_top_right(mi_row, mi_col, bw4, bh4, fctx);
        if do_tr && bw4 != bh4 && tr_reach != has_top_right(mi_row, mi_col, bw4, fctx) {
            hit!(TR_REACH_LONGER_SIDE_HITS);
        }
        if do_tr && tr_reach {
            let tr_col = mi_col + bw4;
            if mi_row > 0
                && tr_col < mi_cols
                && let Some(info) = grid.get(mi_row - 1, tr_col)
                && single_ref_match(&info)
            {
                samples.push(rec(&info, 0, -1, bw4 as i32, 1));
            }
        }
    }
    samples
}

/// `mbmi->num_proj_ref`: [`find_samples`]'s own count, all that
/// `motion_mode_allowed` needs to pick the 3-symbol `motion_mode_cdf` over
/// the 2-symbol `obmc_cdf` (spec 5.11.24).
#[allow(clippy::too_many_arguments)]
pub(crate) fn num_proj_ref(
    grid: &MiGrid,
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_cols: usize,
    mi_rows: usize,
    ref_frame: i8,
    fctx: &crate::decode::FrameCtx,
) -> u8 {
    find_samples(
        grid, mi_row, mi_col, bw4, bh4, mi_cols, mi_rows, ref_frame, fctx,
    )
    .len() as u8
}

/// `av1_get_obmc_mask` (`reconinter.c`): the per-position blend weight
/// (`AOM_BLEND_A64` numerator, out of 64) for the neighbour's own
/// contribution's *complement* -- i.e. `dst = (mask*orig + (64-mask)*nbr +
/// 32) >> 6`, low near the shared border (heavier neighbour weight) rising
/// to 64 (no neighbour contribution) away from it. Only the lengths this
/// decoder's own overlap sizes ever produce (luma 8/16/32, chroma 4/8/16)
/// are transcribed.
fn obmc_mask(len: usize) -> &'static [u8] {
    // lane-gmaffine r1: the two SHORT masks (libaom `reconinter.c`
    // `obmc_mask_1`/`obmc_mask_2`) -- an 8x8 leaf's chroma plane is 4x4, so
    // its overlap is 2 (and 1 at a half-block edge). Unreachable until this
    // round wired interior 16x16 splits down to real 8x8 leaves, where the
    // old `unreachable!()` below fired as a hard panic.
    const M1: [u8; 1] = [64];
    // lane-inter8 r1: `obmc_mask_2` (`reconinter.c` 753) -- reached by the
    // LEFT pass of an 8x8 block's chroma plane (overlap 4 luma columns, 2
    // chroma), which only became live once 8x8 inter leaves stopped being
    // refused.
    const M2: [u8; 2] = [45, 64];
    const M4: [u8; 4] = [39, 50, 59, 64];
    const M8: [u8; 8] = [36, 42, 48, 53, 57, 61, 64, 64];
    const M16: [u8; 16] = [
        34, 37, 40, 43, 46, 49, 52, 54, 56, 58, 60, 61, 64, 64, 64, 64,
    ];
    const M32: [u8; 32] = [
        33, 35, 36, 38, 40, 41, 43, 44, 45, 47, 48, 50, 51, 52, 53, 55, 56, 57, 58, 59, 60, 60, 61,
        62, 64, 64, 64, 64, 64, 64, 64, 64,
    ];
    match len {
        1 => &M1,
        2 => &M2,
        4 => &M4,
        8 => &M8,
        16 => &M16,
        32 => &M32,
        _ => unreachable!("obmc overlap length outside this decoder's own block-size range"),
    }
}

/// A bordering block's own resolved interp filter kernel, `(h, v)` --
/// mirrors [`resolve_interp_filter`]'s own return order from its stored
/// `[u8; 2]` (`Neighbours::above_filter`/`left_filter`, `[sym0, sym1]` =
/// `[v_sym, h_sym]`). The `[3, 3]` sentinel is only ever stored for an intra
/// neighbour (excluded by [`overlappable_above`]/[`overlappable_left`]'s
/// `is_inter` gate) or when `interp_fixed` is `Some` for the whole frame
/// (handled here first) -- so a sentinel reaching the kernel lookup means the
/// stream desynced upstream, and this refuses instead of panicking.
pub(super) fn neighbour_filter(
    interp_fixed: Option<mc::InterpFilterKind>,
    sym: [u8; 2],
) -> Result<(mc::InterpFilterKind, mc::InterpFilterKind)> {
    if let Some(kind) = interp_fixed {
        return Ok((kind, kind));
    }
    // lane-golomb r9: this used to hand the sentinel straight to
    // `from_switchable_symbol`, which PANICS on any value above 2 -- a crash
    // on stream data (measured on a real 192x68 cq40 aomenc stream, where an
    // inter leaf below 8x8 desyncs upstream and leaves an unrecorded filter
    // behind). A decoder must refuse such a stream by name, never abort the
    // process; the desync that produces the sentinel belongs to lane-intersub8.
    if sym[0] > 2 || sym[1] > 2 {
        return Err(unsupported(
            "an OBMC neighbour whose switchable interp filter was never recorded",
        ));
    }
    hit!(OBMC_FILTER_BAND_READS);
    Ok((
        mc::InterpFilterKind::from_switchable_symbol(sym[1] as usize),
        mc::InterpFilterKind::from_switchable_symbol(sym[0] as usize),
    ))
}

/// lane-obmcrec r1 MEASUREMENT rung (`EC_OBMCREC=1`): every OBMC neighbour whose
/// recorded switchable-filter symbols are the `[3, 3]` "never recorded" sentinel
/// -- names the neighbour's own position/shape/reference so the decode path that
/// failed to record its filter can be identified. Read-only, off by default.
#[allow(clippy::too_many_arguments)]
fn obmcrec_probe(
    dir: &str,
    mi_row: usize,
    mi_col: usize,
    write_w: usize,
    write_h: usize,
    off4: usize,
    span4: usize,
    nb: &MiInfo,
    syms: [u8; 2],
    cell: Option<MiInfo>,
    fixed: bool,
) {
    if (syms[0] <= 2 && syms[1] <= 2) || !crate::envflags::env_flag!("EC_OBMCREC") {
        return;
    }
    eprintln!(
        "OBMCREC {dir} cur_mi=({mi_row},{mi_col}) cur_wh=({write_w}x{write_h}) off4={off4} \
span4={span4} syms={syms:?} fixed={fixed} nb_size=({}x{}) nb_ref={} nb_ref1={:?} nb_mv=({},{}) \
nb_newmv={} cell_size={:?} cell_inter={:?} cell_ref={:?} sb_row_off={} sb_col_off={}",
        nb.size * 4,
        nb.size_h * 4,
        nb.ref_frame,
        nb.ref_frame1,
        nb.mv.0,
        nb.mv.1,
        nb.is_new_mv,
        cell.map(|c| (c.size * 4, c.size_h * 4)),
        cell.map(|c| c.is_inter),
        cell.map(|c| c.ref_frame),
        mi_row % 32,
        mi_col % 32,
    );
}

/// One neighbour's own re-prediction over a `w`x`h` region, MC'd fresh
/// against its own `mv`/reference/filter (libaom `av1_build_inter_predictor`
/// under `av1_setup_build_prediction_by_{above,left}_pred`).
#[allow(clippy::too_many_arguments)]
fn obmc_neighbour_pred(
    refplane: &PlaneBuf<'_>,
    x: usize,
    y: usize,
    mv: (i32, i32),
    w: usize,
    h: usize,
    luma: bool,
    h_kind: mc::InterpFilterKind,
    v_kind: mc::InterpFilterKind,
    // lane-scaledref r1: spec 7.11.3.3's scale for THIS neighbour's own
    // reference (derived from luma sizes, applied unchanged to chroma whose
    // x_q4/y_q4 are already in its own plane's pixel units). An unscaled
    // reference takes the ordinary stride-1 path bit-exact.
    scale: mc::RefScale,
    // lane-alloc: the caller keeps one buffer per plane and reuses it across
    // every neighbour of both passes -- this was a fresh `vec![0u16; w * h]`
    // per neighbour per plane.
    out: &mut Vec<u16>,
    fctx: &crate::decode::FrameCtx,
) {
    out.clear();
    out.resize(w * h, 0);
    if scale.is_unscaled() {
        mc::predict_with_filters(
            &refplane.data,
            refplane.width,
            refplane.true_width,
            refplane.true_height,
            mv_to_q4(x, mv.1, if luma { 0 } else { ss_x(fctx) }),
            mv_to_q4(y, mv.0, if luma { 0 } else { ss_y(fctx) }),
            w,
            h,
            h_kind,
            v_kind,
            out,
            fctx,
        );
    } else {
        mc::predict_scaled(
            &refplane.data,
            refplane.width,
            refplane.true_width,
            refplane.true_height,
            mv_to_q4(x, mv.1, if luma { 0 } else { ss_x(fctx) }),
            mv_to_q4(y, mv.0, if luma { 0 } else { ss_y(fctx) }),
            scale,
            w,
            h,
            h_kind,
            v_kind,
            out,
            fctx,
        );
    }
}

/// Blends `tmp` into `dst` (stride `stride`) at `(ox, oy)`, mask varying by
/// row (the above-neighbour pass, `aom_blend_a64_vmask`).
/// `ii_weights1d` (reconinter.c:524): the interintra smooth-mask falloff.
pub(super) const II_WEIGHTS_1D: [u8; 128] = [
    60, 58, 56, 54, 52, 50, 48, 47, 45, 44, 42, 41, 39, 38, 37, 35, 34, 33, 32, 31, 30, 29, 28, 27,
    26, 25, 24, 23, 22, 22, 21, 20, 19, 19, 18, 18, 17, 16, 16, 15, 15, 14, 14, 13, 13, 12, 12, 12,
    11, 11, 10, 10, 10, 9, 9, 9, 8, 8, 8, 8, 7, 7, 7, 7, 6, 6, 6, 6, 6, 5, 5, 5, 5, 5, 4, 4, 4, 4,
    4, 4, 4, 4, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
];

/// lane-interintra r1: non-wedge interintra prediction for one plane
/// (reconinter.c `av1_build_interintra_predictor` + `combine_interintra` +
/// `build_smooth_interintra_mask`). Builds the intra predictor for the
/// block's II mode (DC/V/H/SMOOTH, angle 0, no filter-intra) from `plane`'s
/// own decoded edges at `(x, y)`, then blends it over the inter prediction
/// in `pred`: `comp = (mask*intra + (64-mask)*inter + 32) >> 6`
/// (`aom_blend_a64_mask`, intra as src0). `ii_size_scales[plane_bsize]`
/// reduces to `128 / side` for the square plane sizes this decoder reaches.
/// lane-wii r2: `wedge == Some(..)` replaces the mode mask with the wedge
/// codebook mask (fixed sign 0; luma stride, 2x2 box-averaged for chroma).
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn interintra_blend(
    plane: &PlaneBuf<'_>,
    x: usize,
    y: usize,
    bw: usize,
    bh: usize,
    stride: usize,
    subw: usize,
    subh: usize,
    ii_mode: u8,
    wedge: Option<(&'static [u8], usize)>,
    pred: &mut [u16],
    fctx: &crate::decode::FrameCtx,
) {
    // `interintra_to_intra_mode`: II_DC/II_V/II_H/II_SMOOTH.
    let intra_mode = match ii_mode {
        0 => crate::intra::DC_PRED,
        1 => crate::intra::V_PRED,
        2 => crate::intra::H_PRED,
        _ => crate::intra::SMOOTH_PRED,
    };
    let (above, left, corner) = plane.edges_rect(
        x,
        y,
        bw,
        bh,
        crate::encode::Reach {
            above_right: false,
            below_left: false,
        },
    );
    let mut intra = vec![0u16; bw * bh];
    // Edge filtering never applies here: V/H at delta 0 are plain edge
    // copies (angle 90/180 skip the directional walk), DC/SMOOTH carry no
    // angle at all.
    predict(
        intra_mode,
        0,
        above.as_deref(),
        left.as_deref(),
        corner,
        bw,
        bh,
        false,
        false,
        &mut intra,
        fctx,
    );
    // `ii_size_scales[bsize]` is keyed on the block's LONGER side
    // (`MAX_SB_SIZE >> max(bw,bh)` order), not on the square this decoder
    // builds the predictor buffer at.
    let scale = 128 / bw.max(bh);
    for i in 0..bh {
        for j in 0..bw {
            let m = match wedge {
                // lane-wii r2: wedge-interintra mask (aom_blend_a64_mask
                // over the wedge codebook, intra as src0, fixed sign 0).
                // `(subw, subh)` are the plane's own subsampling shifts;
                // the four branches mirror aom_dsp/blend-a64-mask.c's
                // `subw`/`subh` pair handling exactly: luma (0, 0) reads
                // the direct sample, (1, 1) the 2x2 box average
                // (reconinter.c sub8x8), and 4:2:2's (1, 0) the
                // `AOM_BLEND_AVG` column pair.
                Some((mask, ms)) => match (subw, subh) {
                    (0, 0) => u32::from(mask[i * ms + j]),
                    (1, 1) => {
                        let t = 2 * i * ms + 2 * j;
                        (u32::from(mask[t])
                            + u32::from(mask[t + 1])
                            + u32::from(mask[t + ms])
                            + u32::from(mask[t + ms + 1])
                            + 2)
                            >> 2
                    }
                    (1, 0) => {
                        let t = i * ms + 2 * j;
                        (u32::from(mask[t]) + u32::from(mask[t + 1]) + 1) >> 1
                    }
                    (_sw, _sh) => {
                        let t = 2 * i * ms + j;
                        (u32::from(mask[t]) + u32::from(mask[t + ms]) + 1) >> 1
                    }
                },
                None => u32::from(match ii_mode {
                    0 => 32u8,
                    1 => II_WEIGHTS_1D[i * scale],
                    2 => II_WEIGHTS_1D[j * scale],
                    _ => II_WEIGHTS_1D[i.min(j) * scale],
                }),
            };
            let idx = i * stride + j;
            pred[idx] = ((m * u32::from(intra[i * bw + j]) + (64 - m) * u32::from(pred[idx]) + 32)
                >> 6) as u16;
        }
    }
}

/// `w` is the number of columns actually written (clipped to the block's own
/// buffer at a frame edge); `ts` is `tmp`'s own stride, the neighbour
/// prediction's full width.
#[allow(clippy::too_many_arguments)]
fn obmc_blend_v(
    dst: &mut [u16],
    stride: usize,
    ox: usize,
    oy: usize,
    w: usize,
    h: usize,
    ts: usize,
    tmp: &[u16],
) {
    let mask = obmc_mask(h);
    for row in 0..h {
        let m = u32::from(mask[row]);
        for col in 0..w {
            let d = &mut dst[(oy + row) * stride + ox + col];
            let t = u32::from(tmp[row * ts + col]);
            *d = ((m * u32::from(*d) + (64 - m) * t + 32) >> 6) as u16;
        }
    }
}

/// Blends `tmp` into `dst` at `(ox, oy)`, mask varying by column (the
/// left-neighbour pass, `aom_blend_a64_hmask`).
fn obmc_blend_h(
    dst: &mut [u16],
    stride: usize,
    ox: usize,
    oy: usize,
    w: usize,
    h: usize,
    tmp: &[u16],
) {
    let mask = obmc_mask(w);
    for row in 0..h {
        for col in 0..w {
            let m = u32::from(mask[col]);
            let d = &mut dst[(oy + row) * stride + ox + col];
            let t = u32::from(tmp[row * w + col]);
            *d = ((m * u32::from(*d) + (64 - m) * t + 32) >> 6) as u16;
        }
    }
}

/// libaom's own `BLOCK_SIZE` enum index for a `w4`x`h4` (mi-unit) block --
/// only for the `EC_OBMC` trace rung below, so it reads byte-identical to
/// the instrumented aomdec's `backup_mbmi.bsize`.
fn ec_obmc_bsize(w4: usize, h4: usize) -> i32 {
    match (w4, h4) {
        (1, 1) => 0,
        (1, 2) => 1,
        (2, 1) => 2,
        (2, 2) => 3,
        (2, 4) => 4,
        (4, 2) => 5,
        (4, 4) => 6,
        (4, 8) => 7,
        (8, 4) => 8,
        (8, 8) => 9,
        (8, 16) => 10,
        (16, 8) => 11,
        (16, 16) => 12,
        (1, 4) => 16,
        (4, 1) => 17,
        (2, 8) => 18,
        (8, 2) => 19,
        (4, 16) => 20,
        (16, 4) => 21,
        _ => -1,
    }
}

/// The `EC_OBMC` rung: one line per blended neighbour, in the instrumented
/// aomdec's own byte format (`decodeframe.c`
/// `dec_build_prediction_by_{above,left}_pred`), so the two decoders' OBMC
/// neighbour lists diff line for line. `filt` is libaom's packed
/// `interp_filters.as_int` = `y_filter | (x_filter << 16)`.
#[allow(clippy::too_many_arguments)]
fn ec_obmc_trace(
    dir: &str,
    mi_row: usize,
    mi_col: usize,
    write_w: usize,
    write_h: usize,
    rel: usize,
    op: usize,
    nb: &MiInfo,
    h_kind: mc::InterpFilterKind,
    v_kind: mc::InterpFilterKind,
) {
    if !crate::envflags::env_flag!("EC_OBMC") {
        return;
    }
    let sym = |k: mc::InterpFilterKind| match k {
        mc::InterpFilterKind::Smooth => 1u32,
        mc::InterpFilterKind::Sharp => 2,
        _ => 0,
    };
    eprintln!(
        "EC_OBMC {dir} mi=({mi_row},{mi_col}) wh=({write_w}x{write_h}) rel={rel} op={op} \
nbmv=({},{}) nbref={} nbbsize={} filt={}",
        nb.mv.0,
        nb.mv.1,
        nb.ref_frame,
        ec_obmc_bsize(nb.size as usize, nb.size_h as usize),
        sym(v_kind) | (sym(h_kind) << 16),
    );
}

/// `av1_build_obmc_inter_prediction` (libaom `reconinter.c`): re-predicts
/// the overlap strip against each bordering above/left inter neighbour's own
/// mv/ref/filter and blends it into this block's just-built single-reference
/// prediction, luma then both 4:2:0 chroma planes -- above pass first, then
/// left, each reading only the *original* prediction this function was
/// handed (never the other pass's own output), matching libaom's own
/// independence between the two. `max_neighbor_obmc`'s own cap table
/// (`{0,1,2,3,4,4}` indexed by `mi_size_wide_log2`/`_high_log2`, this
/// decoder's own square blocks always index the same value both ways) bounds
/// how many bordering blocks actually get blended, separate from the
/// eligibility scan's own unbounded ("any at all") walk.
#[allow(clippy::too_many_arguments)]
pub(crate) fn obmc_plan(
    grid: &MiGrid,
    // The two switchable-filter bands (`Neighbours::above_filter`/
    // `left_filter`), read only when `interp_fixed` is `None`. The ENCODER
    // (`encode.rs`'s OBMC candidate) codes a fixed interpolation filter and
    // has no `Neighbours` of its own, so it passes both empty; the reads
    // below fall back to the `[3, 3]` never-recorded sentinel, which
    // `neighbour_filter` never reaches under a fixed filter.
    // corner-cut: ceiling is a switchable-filter ENCODER -- it must build and
    // pass the real bands here, exactly as the decoder does.
    above_filter: &[[u8; 2]],
    left_filter: &[[u8; 2]],
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    mi_rows: usize,
    mi_cols: usize,
    side: usize,
    write_w: usize,
    write_h: usize,
    // lane-av1-llpred2: the FRAME's own subsampling pair -- the same pair the
    // chroma window half in [`obmc_run`] shifts by. A rect strip's ss cannot
    // be derived from a rect size over the square `chroma_side`: that ratio
    // gave a 16x8 strip at 4:2:0 ss=(1,0) (losing its above-chroma skip) and
    // divides by zero at 4:4:4 (`trailing_zeros(0)` = 64, a debug shift panic).
    ss_x: usize,
    ss_y: usize,
    chroma_stride: usize,
    px: usize,
    py: usize,
    cpx: usize,
    cpy: usize,
    refpix: &RefPix<'_>,
    interp_fixed: Option<mc::InterpFilterKind>,
    // lane-scaledref r1: this frame's own coded luma width -- each OBMC
    // neighbour is re-predicted against ITS OWN reference, which may be
    // scaled differently from this block's (spec 7.11.3.3 / libaom
    // `av1_setup_build_prediction_by_above_pred` passing that ref's own
    // `scale_factors`).
    frame_size: (usize, usize),
) -> Result<ObmcPlan> {
    // lane-rect r2 (libaom av1_build_obmc_inter_prediction): each pass has
    // its OWN neighbour cap (width-log2 for above, height-log2 for left) and
    // overlap (bh/2 rows above, bw/2 cols left) -- one square `side` was
    // right only for square blocks.
    // libaom `max_neighbor_obmc[6] = { 0, 1, 2, 3, 4, 4 }` indexed by
    // `mi_size_wide_log2[bsize]` (above pass) / `mi_size_high_log2` (left):
    // 1 mi -> 0, 2 -> 1, 4 -> 2, 8 -> 3, 16+ -> 4. lane-inter4 r3: the old
    // `1..=4 => 2` row was right only for blocks at least 16 px on that axis
    // -- a 16x8 leaf's LEFT pass (bh4 = 2) walked up to two neighbours where
    // libaom walks one, blending an extra 8-px-tall neighbour into every
    // rect leaf with two of them on its left edge (class table-narrowed-to-
    // the-reachable-sizes; the square gates could never expose it).
    let max_nb = |n4: usize| match n4 {
        0..=1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        _ => 4,
    };
    // lane-inter4 r4: r3 refused OBMC on a 16x8/8x16 leaf; re-measured
    // byte-exact against an instrumented aomdec, the refusal is gone and the
    // counter below is what the gate's `--enable-obmc=1` arm reads.
    if matches!((write_w, write_h), (16, 8) | (8, 16)) {
        hit!(OBMC_RECT_LEAF_HITS);
    }
    // lane-sb128c r1: libaom caps the overlap strip at a 64x64 block's own
    // half (`reconinter.c` 860/899), inert below 128.
    let overlap_above = write_h.min(64) / 2;
    let overlap_left = write_w.min(64) / 2;
    // lane-inter8 r1 / lane-scaledref r1: `av1_skip_u4x4_pred_in_obmc`
    // (`reconinter.c` 829) switches on the PLANE block
    // (`get_plane_block_size`, ss-shifted): only 4x4/8x4/4x8 skip the ABOVE
    // pass's chroma (`return dir == 0`), the LEFT pass still blends them.
    // lane-av1-llpred: the old luma-shape list `(8,8)|(16,8)|(8,16)`
    // hardcoded the 4:2:0 plane sizes; at 4:4:4 the plane block is the luma
    // block itself (never 4-wide), so every one of those blocks was silently
    // missing its above-pass chroma OBMC blend.
    // lane-av1-llpred2: ss is the FRAME's own pair (`ss_x`/`ss_y(fctx)`, the
    // same values [`obmc_run`]'s chroma windows shift by), never a ratio of
    // the rect write size over the square `chroma_side` -- that ratio gave a
    // 4:2:0 16x8 strip ss=(1,0) and dropped its above-chroma skip, and at
    // 4:4:4 divided to zero (`trailing_zeros(0)` = 64, a debug shift panic).
    let skip_chroma_above = obmc_skip_chroma_above(write_w, write_h, ss_x, ss_y);
    let mut above = Vec::new();
    for (off4, span4, nb, src4) in
        overlappable_above(grid, mi_row, mi_col, bw4, mi_cols, max_nb(bw4))
    {
        // `Neighbours::above_filter` is indexed in `SUB`(16px)-wide columns,
        // one step coarser than the mi(4px) units `overlappable_above` walks
        // in -- divide back down to that column. lane-gmaffine r2: the
        // neighbour's OWN mi-granular filter (libaom
        // `av1_setup_build_prediction_by_above_pred` reads
        // `above_mbmi->interp_filters`).
        let above_syms = above_filter.get(mi_col + src4).copied().unwrap_or([3; 2]);
        obmcrec_probe(
            "above",
            mi_row,
            mi_col,
            write_w,
            write_h,
            off4,
            span4,
            &nb,
            above_syms,
            grid.get(mi_row - 1, mi_col + src4),
            interp_fixed.is_some(),
        );
        let (h_kind, v_kind) = neighbour_filter(interp_fixed, above_syms)?;
        ec_obmc_trace(
            "above", mi_row, mi_col, write_w, write_h, off4, span4, &nb, h_kind, v_kind,
        );
        let nb_dims = ref_dims(nb.ref_frame, refpix)?;
        if mi_col + off4 + span4 > mi_cols {
            hit!(OBMC_EDGE_SPAN_HITS);
        }
        above.push(ObmcNb {
            off4,
            span4,
            mv: mv32(nb.mv),
            ref_frame: nb.ref_frame,
            h_kind,
            v_kind,
            scale: scale_of(nb_dims, frame_size),
        });
    }
    let mut left = Vec::new();
    for (off4, span4, nb, src4) in
        overlappable_left(grid, mi_row, mi_col, bh4, mi_rows, max_nb(bh4))
    {
        let left_syms = left_filter.get(mi_row + src4).copied().unwrap_or([3; 2]);
        obmcrec_probe(
            "left",
            mi_row,
            mi_col,
            write_w,
            write_h,
            off4,
            span4,
            &nb,
            left_syms,
            grid.get(mi_row + src4, mi_col - 1),
            interp_fixed.is_some(),
        );
        let (h_kind, v_kind) = neighbour_filter(interp_fixed, left_syms)?;
        ec_obmc_trace(
            "left", mi_row, mi_col, write_w, write_h, off4, span4, &nb, h_kind, v_kind,
        );
        let nb_dims = ref_dims(nb.ref_frame, refpix)?;
        if mi_row + off4 + span4 > mi_rows {
            hit!(OBMC_EDGE_SPAN_HITS);
        }
        left.push(ObmcNb {
            off4,
            span4,
            mv: mv32(nb.mv),
            ref_frame: nb.ref_frame,
            h_kind,
            v_kind,
            scale: scale_of(nb_dims, frame_size),
        });
    }
    Ok(ObmcPlan {
        above,
        left,
        side,
        chroma_stride,
        write_w,
        write_h,
        px,
        py,
        cpx,
        cpy,
        overlap_above,
        overlap_left,
        skip_chroma_above,
    })
}

/// `av1_skip_u4x4_pred_in_obmc`'s above-pass decision (libaom
/// `reconinter.c` 829): the PLANE block (`write_w >> ss_x`, `write_h >>
/// ss_y`) is 4x4, 8x4 or 4x8. `ss_x`/`ss_y` are the FRAME's own pair -- the
/// same values [`obmc_run`]'s chroma windows shift by -- never a ratio over
/// the square `chroma_side` (lane-av1-llpred2: that ratio dropped the
/// 4:2:0 16x8/8x16 skip and shifted by 64 at 4:4:4). Pure so the check
/// below pins the rect-strip decision without decoding a stream.
pub(crate) fn obmc_skip_chroma_above(
    write_w: usize,
    write_h: usize,
    ss_x: usize,
    ss_y: usize,
) -> bool {
    matches!((write_w >> ss_x, write_h >> ss_y), (4, 4) | (8, 4) | (4, 8))
}

/// One OBMC neighbour as the PARSE thread resolved it (lane-defer8): every
/// value [`obmc_run`] needs that lives in the mode-info grid or the
/// neighbour bands, which the parse keeps overwriting as it walks on.
pub(super) struct ObmcNb {
    pub(super) off4: usize,
    pub(super) span4: usize,
    pub(super) mv: (i32, i32),
    pub(super) ref_frame: i8,
    pub(super) h_kind: mc::InterpFilterKind,
    pub(super) v_kind: mc::InterpFilterKind,
    /// spec 7.11.3.3's scale for THIS neighbour's own reference.
    pub(super) scale: mc::RefScale,
}

/// [`obmc_plan`]'s result: the two neighbour passes plus this block's own
/// geometry, everything [`obmc_run`] reads apart from reference PIXELS.
pub(crate) struct ObmcPlan {
    pub(super) above: Vec<ObmcNb>,
    pub(super) left: Vec<ObmcNb>,
    pub(super) side: usize,
    /// The chroma PREDICTION buffer's own stride -- `chroma_stride`, the PLANE
    /// BLOCK's per-axis width, NOT the enclosing square `chroma_side`. At
    /// 4:2:2 that buffer is `(side/2, side)` (lane-av1-422blockhalf), and
    /// striding it with `chroma_side` put row `r` at `r*side`: a 32x16 luma
    /// strip's left-pass chroma blend wrote row 15 at index 15*32 = 480 of a
    /// 256-sample `16 x 16` buffer -- the out-of-bounds panic in
    /// `obmc_blend_h` the pinned 4:2:2 rotzoom stream opened. Every other
    /// consumer of `pred_u`/`pred_v` in this block already strides with
    /// `chroma_stride`; at 4:2:0 and 4:4:4 the two are equal.
    pub(super) chroma_stride: usize,
    pub(super) write_w: usize,
    pub(super) write_h: usize,
    pub(super) px: usize,
    pub(super) py: usize,
    pub(super) cpx: usize,
    pub(super) cpy: usize,
    pub(super) overlap_above: usize,
    pub(super) overlap_left: usize,
    pub(super) skip_chroma_above: bool,
}

/// The reference-pixel half of OBMC: re-predicts every planned neighbour
/// over this block's own edge strips and blends it into the prediction
/// (libaom `av1_build_obmc_inter_prediction`). Reads nothing but reference
/// pixels and the plan, so it runs wherever the block's prediction is built
/// -- at the prediction's own push site (lane-defer8).
pub(crate) fn obmc_run(
    plan: &ObmcPlan,
    refpix: &RefPix<'_>,
    pred_y: &mut [u16],
    pred_u: &mut [u16],
    pred_v: &mut [u16],
    fctx: &crate::decode::FrameCtx,
) {
    let &ObmcPlan {
        side,
        chroma_stride,
        write_w,
        write_h,
        px,
        py,
        cpx,
        cpy,
        overlap_above,
        overlap_left,
        skip_chroma_above,
        ..
    } = plan;
    // lane-alloc: one neighbour-prediction buffer per plane for the whole
    // block, reused by every neighbour of the above and left passes.
    let (mut tmp_y, mut tmp_u, mut tmp_v) = (Vec::new(), Vec::new(), Vec::new());

    // lane-t900 r13 rung: EC_MCB="<plane>:<px>:<py>[:<frame>]" (chroma plane
    // 1 only; <px>:<py> are chroma-plane pixels, the oracle's own dump
    // window) prints this block's chroma prediction rows before and after the
    // OBMC blend, matching the instrumented aomdec's EC_MCB dump.
    let ec_mcb: Option<(usize, usize, i64)> = crate::envflags::env_flag!("EC_MCB")
        .then(|| crate::envflags::var("EC_MCB").ok())
        .flatten()
        .and_then(|v| {
            let f: Vec<&str> = v.split(':').collect();
            if f.len() < 3 || f[0] != "1" {
                return None;
            }
            let (x, y) = (f[1].parse::<usize>().ok()?, f[2].parse::<usize>().ok()?);
            let fr = f.get(3).and_then(|s| s.parse::<i64>().ok()).unwrap_or(-1);
            let idx = PREFILT_PICTURE_IDX.load(std::sync::atomic::Ordering::SeqCst) as i64;
            // lane-av1ecmcb: the window is the CHROMA plane's own write region --
            // the oracle's EC_MCB spans are plane pixels, and at 4:4:4 (ss 0/0)
            // the chroma block is full-size, so nothing halves. The old
            // `write_w / 2` was a 4:2:0-era assumption that quartered the window
            // (and the dumps below) at ss 0/0.
            let (cw, ch) = (write_w >> ss_x(fctx), write_h >> ss_y(fctx));
            if x >= cpx && x < cpx + cw && y >= cpy && y < cpy + ch && (fr < 0 || fr == idx) {
                Some((x, y, idx))
            } else {
                None
            }
        });
    if let Some((_, _, idx)) = ec_mcb {
        let (cw, ch) = (write_w >> ss_x(fctx), write_h >> ss_y(fctx));
        eprintln!("OUR_MCB pre f={idx} cpx={cpx} cpy={cpy} cw={cw} ch={ch}");
        for r in 0..ch {
            let row: Vec<u16> = (0..cw).map(|c| pred_u[r * chroma_stride + c]).collect();
            eprintln!("OUR_MCB prerow{r}: {row:?}");
        }
    }
    for nb in &plan.above {
        let Some((ny, nu, nv)) = ref_planes_deferred(nb.ref_frame, refpix) else {
            return;
        };
        let (bw, bh, ox) = (nb.span4 * 4, overlap_above, nb.off4 * 4);
        // lane-t900 r14: the prediction is `op_mi_size` wide (kernel choice
        // included); only the WRITE is clipped to this block's own buffer.
        obmc_neighbour_pred(
            ny,
            px + ox,
            py,
            nb.mv,
            bw,
            bh,
            true,
            nb.h_kind,
            nb.v_kind,
            nb.scale,
            &mut tmp_y,
            fctx,
        );
        obmc_blend_v(pred_y, side, ox, 0, bw.min(write_w - ox), bh, bw, &tmp_y);
        if !skip_chroma_above {
            // lane-t900 r13 (measured, NOT a defect): libaom's min-4 clamp
            // sizes only the OBMC PREDICTION buffer; the blend length is the
            // plain overlap (`build_obmc_inter_pred_above`:
            // `overlap >> ss_y`) -- the PLANE's own subsampling shift, 0 at
            // 4:4:4 (lane-av1-llpred: the old `/2` hardcoded the 4:2:0 half
            // and half-blended every 4:4:4 OBMC block's chroma).
            let (cbw, cbh, cox) = (
                bw >> ss_x(fctx),
                overlap_above >> ss_y(fctx),
                ox >> ss_x(fctx),
            );
            let cw = cbw.min((write_w >> ss_x(fctx)) - cox);
            obmc_neighbour_pred(
                nu,
                cpx + cox,
                cpy,
                nb.mv,
                cbw,
                cbh,
                false,
                nb.h_kind,
                nb.v_kind,
                nb.scale,
                &mut tmp_u,
                fctx,
            );
            obmc_blend_v(pred_u, chroma_stride, cox, 0, cw, cbh, cbw, &tmp_u);
            obmc_neighbour_pred(
                nv,
                cpx + cox,
                cpy,
                nb.mv,
                cbw,
                cbh,
                false,
                nb.h_kind,
                nb.v_kind,
                nb.scale,
                &mut tmp_v,
                fctx,
            );
            obmc_blend_v(pred_v, chroma_stride, cox, 0, cw, cbh, cbw, &tmp_v);
        }
    }

    for nb in &plan.left {
        let Some((ny, nu, nv)) = ref_planes_deferred(nb.ref_frame, refpix) else {
            return;
        };
        let (bw, bh, oy) = (overlap_left, nb.span4 * 4, nb.off4 * 4);
        obmc_neighbour_pred(
            ny,
            px,
            py + oy,
            nb.mv,
            bw,
            bh,
            true,
            nb.h_kind,
            nb.v_kind,
            nb.scale,
            &mut tmp_y,
            fctx,
        );
        obmc_blend_h(pred_y, side, 0, oy, bw, bh.min(write_h - oy), &tmp_y);
        let (cbw, cbh, coy) = (
            overlap_left >> ss_x(fctx),
            bh >> ss_y(fctx),
            oy >> ss_y(fctx),
        );
        obmc_neighbour_pred(
            nu,
            cpx,
            cpy + coy,
            nb.mv,
            cbw,
            cbh,
            false,
            nb.h_kind,
            nb.v_kind,
            nb.scale,
            &mut tmp_u,
            fctx,
        );
        if let Some((_, _, idx)) = ec_mcb {
            eprintln!(
                "OUR_MCB left f={idx} off4={} span4={} cbw={cbw} cbh={cbh} coy={coy} mv=({},{}) ref0={}",
                nb.off4, nb.span4, nb.mv.0, nb.mv.1, nb.ref_frame
            );
            for r in 0..cbh {
                let row: Vec<u16> = (0..cbw).map(|c| tmp_u[r * cbw + c]).collect();
                eprintln!("OUR_MCB lrow{r}: {row:?}");
            }
        }
        let ch = cbh.min((write_h >> ss_y(fctx)) - coy);
        obmc_blend_h(pred_u, chroma_stride, 0, coy, cbw, ch, &tmp_u);
        obmc_neighbour_pred(
            nv,
            cpx,
            cpy + coy,
            nb.mv,
            cbw,
            cbh,
            false,
            nb.h_kind,
            nb.v_kind,
            nb.scale,
            &mut tmp_v,
            fctx,
        );
        obmc_blend_h(pred_v, chroma_stride, 0, coy, cbw, ch, &tmp_v);
    }
    if let Some((_, _, idx)) = ec_mcb {
        let (cw, ch) = (write_w >> ss_x(fctx), write_h >> ss_y(fctx));
        for r in 0..ch {
            let row: Vec<u16> = (0..cw).map(|c| pred_u[r * chroma_stride + c]).collect();
            eprintln!("OUR_MCB post f={idx} row{r}: {row:?}");
        }
    }
}

/// `is_any_masked_compound_used` (libaom `reconinter.h`), specialised to
/// the square block sizes this decoder ever decodes (32/16/8, all
/// `min(w,h) >= 8`, `is_comp_ref_allowed` true): `COMPOUND_DIFFWTD` is then
/// always `is_interinter_compound_used`, so the function is always `true`
/// for `side` -- kept as a named call (rather than inlined `true`) so a
/// future sub-8 block size does not silently misdecode a real stream.
pub(super) fn is_any_masked_compound_used_here(bw: usize, bh: usize) -> bool {
    // lane-inter16ab r4 (class sweep of the r3 CDF-row defect):
    // `is_comp_ref_allowed(bsize)` is `AOMMIN(block_size_wide, block_size_high)
    // >= 8` on the block's TRUE footprint -- a 16x4/4x16 strip is NOT allowed,
    // so libaom writes no `comp_group_idx` symbol there at all. Keyed off the
    // square `side` we read one the reference never wrote.
    bw.min(bh) >= 8
}

/// The `BLOCK_SIZES_ALL` index of a `bw x bh` block (libaom `enum BLOCK_SIZE`,
/// `blockd.h`) -- the row every size-indexed inter CDF table
/// (`compound_type_cdf`, `wedge_idx_cdf`) is keyed by. Our `decode_inter_block`
/// carries the enclosing SQUARE `side` for context purposes (the accepted
/// strip corner-cut), so a 16x8 / 8x16 PARTITION_HORZ/VERT strip that reads
/// one of those symbols off `side` alone takes BLOCK_16X16's row and desyncs
/// (lane-inter16ab r3: a real 16x8 compound strip with `comp_group_idx == 1`,
/// decode-order frame 5, our range 64901 vs the oracle's 47271 at the very
/// next block's mode read).
pub(super) fn bsize_all_index(bw: usize, bh: usize) -> Option<usize> {
    Some(match (bw, bh) {
        (4, 4) => 0,
        (4, 8) => 1,
        (8, 4) => 2,
        (8, 8) => 3,
        (8, 16) => 4,
        (16, 8) => 5,
        (16, 16) => 6,
        (16, 32) => 7,
        (32, 16) => 8,
        (32, 32) => 9,
        (32, 64) => 10,
        (64, 32) => 11,
        (64, 64) => 12,
        (64, 128) => 13,
        (128, 64) => 14,
        (128, 128) => 15,
        (4, 16) => 16,
        (16, 4) => 17,
        (8, 32) => 18,
        (32, 8) => 19,
        (16, 64) => 20,
        (64, 16) => 21,
        _ => return None,
    })
}

/// `av1_wedge_params_lookup[bsize].wedge_types > 0` (libaom `reconinter.c`):
/// the block sizes that carry a wedge codebook at all. A `compound_type`
/// symbol is written only for these; every other size infers
/// `COMPOUND_DIFFWTD`.
pub(super) fn wedge_used_bsize(bw: usize, bh: usize) -> bool {
    matches!(
        (bw, bh),
        (8, 8) | (8, 16) | (16, 8) | (16, 16) | (16, 32) | (32, 16) | (32, 32) | (8, 32) | (32, 8)
    )
}

/// Whether a compound reference pair is unidirectional (both references on
/// the same temporal side of the current frame) -- `has_uni_comp_refs`
/// (libaom `av1_reference_frame_utils.h`/`pred_common.c`'s own definition:
/// a pair is `UNIDIR_COMP_REFERENCE` exactly when neither reference is
/// backward-while-the-other-is-forward, i.e. `is_backward(ref0) ==
/// is_backward(ref1)`). Used to build a real
/// [`crate::mvstack::NeighbourRef::uni`] once `ref1` is known, matching
/// [`crate::mvstack::comp_reference_type_ctx`]'s own `is_backward` calls.
pub(super) fn is_uni_comp_ref(ref0: i8, ref1: i8) -> bool {
    let backward = |r: i8| (BWDREF_FRAME..=ALTREF_FRAME).contains(&r);
    backward(ref0) == backward(ref1)
}

/// `get_comp_group_idx_context` (libaom `pred_common.h`): sums the
/// above/left neighbour contributions [`Neighbours::record_inter`]/
/// [`Neighbours::record_compound_ctx`] already precomputed, clamped to 5.
/// lane-inter8 r2: `at_mi` is the block's own top-left in 4x4 mi units --
/// an 8x8 leaf used to pass its enclosing 16x16's corner here, so all four
/// leaves read the same neighbour cell (class context-read-from-one-cell).
pub(super) fn get_comp_group_idx_context(
    neighbours: &Neighbours,
    at_mi: (usize, usize),
    side: usize,
) -> usize {
    let _ = side;
    let (rmi, cmi) = at_mi;
    (neighbours.above_comp_group_idx[cmi] as usize + neighbours.left_comp_group_idx[rmi] as usize)
        .min(5)
}

/// `get_comp_index_context` (libaom `pred_common.h`): the above/left
/// neighbour bits plus `3 * (fwd == bck)`, where `fwd`/`bck` are this
/// block's own two references' distances from the current frame (spec
/// 7.11.3.15's `get_relative_dist`, same convention
/// [`crate::compound::dist_wtd_comp_weight_assign`] uses).
#[allow(clippy::too_many_arguments)]
pub(super) fn get_comp_index_context(
    neighbours: &Neighbours,
    at_mi: (usize, usize),
    side: usize,
    order_hint_bits: u32,
    cur_order_hint: u32,
    ref0_order_hint: u32,
    ref1_order_hint: u32,
) -> usize {
    let _ = side;
    let (rmi, cmi) = at_mi;
    let fwd =
        crate::motion_field::get_relative_dist(order_hint_bits, ref1_order_hint, cur_order_hint)
            .abs();
    let bck =
        crate::motion_field::get_relative_dist(order_hint_bits, cur_order_hint, ref0_order_hint)
            .abs();
    let offset = usize::from(fwd == bck);
    neighbours.above_compound_idx[cmi] as usize
        + neighbours.left_compound_idx[rmi] as usize
        + 3 * offset
}

pub(super) fn read_comp_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above: Option<NeighbourRef>,
    left: Option<NeighbourRef>,
) -> bool {
    let ctx = reference_mode_ctx(above, left);
    let compound = dec.symbol(&mut cdfs.comp_mode[ctx]) == 1;
    if crate::envflags::env_flag!("EC_TRACE_MODE") {
        eprintln!(
            "EC_RCM ctx={ctx} val={} a={:?} l={:?} rng={}",
            usize::from(compound),
            above.map(|n| (n.is_inter, n.ref0, n.ref1)),
            left.map(|n| (n.is_inter, n.ref0, n.ref1)),
            dec.debug_state().0
        );
    }
    if compound {
        hit!(COMP_MODE_HITS);
    }
    compound
}

/// Spec 5.11.25's `comp_reference_type` + the `uni_comp_ref`/`comp_ref`/
/// `comp_bwdref` trees (libaom `decodemv.c`'s `read_ref_frames` compound
/// arm), reached once [`read_comp_mode`] has already returned
/// `COMPOUND_REFERENCE`. Returns `(RefFrame[0], RefFrame[1])`.
/// `above`/`left` are [`comp_reference_type_ctx`]'s own `NeighbourRef`
/// convention; `above_ref`/`left_ref` are the scalar `1..=7`/`-1`
/// convention [`read_single_ref`] already uses, for every downstream binary
/// decision. libaom `pred_common.c`'s own compound context functions
/// (`av1_get_pred_context_uni_comp_ref_p[1|2]`, `comp_ref_p[1|2]`,
/// `comp_bwdref_p[1]`) turned out to be exactly [`single_ref_p1_ctx`]..
/// [`single_ref_p6_ctx`]/[`uni_comp_ref_p1_ctx`] under a different name
/// (same vote-the-neighbours-above-left shape, same forward/backward and
/// LAST/LAST2/LAST3/GOLDEN groupings) -- lane-av1comp audited each one
/// against the C source rather than porting six near-duplicate functions.
/// Its caller today only needs the symbols consumed, correctly, to keep the
/// arithmetic decoder's own state in sync before refusing the block by name
/// -- the returned pair is not yet threaded into any motion-compensation
/// path.
pub(super) fn read_compound_ref_frames(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    above: Option<NeighbourRef>,
    left: Option<NeighbourRef>,
    above_ref: i8,
    left_ref: i8,
) -> (i8, i8) {
    let a = (above_ref > 0).then_some(above_ref);
    let l = (left_ref > 0).then_some(left_ref);
    // libaom `av1_collect_neighbors_ref_counts` counts BOTH of a compound
    // neighbour's references, not just its first -- `a1`/`l1` carry that
    // second reference through to every vote below (lane-av1blend r6: a
    // missing `a1`/`l1` here was decoding `compound_idx` off the wrong CDF
    // row for a block sitting under a skip_mode-forced compound neighbour).
    let a1 = above.and_then(|n| n.ref1);
    let l1 = left.and_then(|n| n.ref1);
    let type_ctx = comp_reference_type_ctx(above, left);
    let unidir = dec.symbol(&mut cdfs.comp_ref_type[type_ctx]) == 0;
    if unidir {
        hit!(UNI_COMP_HITS);
        // uni_comp_ref (p0): forward vs. backward -- av1_get_pred_context_-
        // uni_comp_ref_p is single_ref_p1's own forward/backward vote.
        let bit = dec.symbol(&mut cdfs.uni_comp_ref[single_ref_p1_ctx(a, a1, l, l1)][0]);
        if bit == 1 {
            (BWDREF_FRAME, ALTREF_FRAME)
        } else {
            let p1 = dec.symbol(&mut cdfs.uni_comp_ref[uni_comp_ref_p1_ctx(a, a1, l, l1)][1]);
            if p1 == 1 {
                // uni_comp_ref_p2: LAST3 vs. GOLDEN, same vote as single_ref_p5.
                let p2 = dec.symbol(&mut cdfs.uni_comp_ref[single_ref_p5_ctx(a, a1, l, l1)][2]);
                if p2 == 1 {
                    (LAST_FRAME, GOLDEN_FRAME)
                } else {
                    (LAST_FRAME, LAST3_FRAME)
                }
            } else {
                (LAST_FRAME, LAST2_FRAME)
            }
        }
    } else {
        // comp_ref (p0): LAST/LAST2 vs. LAST3/GOLDEN, same vote as single_ref_p3.
        let bit0 = dec.symbol(&mut cdfs.comp_ref[single_ref_p3_ctx(a, a1, l, l1)][0]);
        let ref0 = if bit0 == 0 {
            // comp_ref_p1: LAST vs. LAST2, same vote as single_ref_p4.
            let p1 = dec.symbol(&mut cdfs.comp_ref[single_ref_p4_ctx(a, a1, l, l1)][1]);
            if p1 == 1 { LAST2_FRAME } else { LAST_FRAME }
        } else {
            // comp_ref_p2: LAST3 vs. GOLDEN, same vote as single_ref_p5.
            let p2 = dec.symbol(&mut cdfs.comp_ref[single_ref_p5_ctx(a, a1, l, l1)][2]);
            if p2 == 1 { GOLDEN_FRAME } else { LAST3_FRAME }
        };
        // comp_bwdref (p0): BWDREF/ALTREF2 vs. ALTREF, same vote as single_ref_p2.
        let bit1 = dec.symbol(&mut cdfs.comp_bwdref[single_ref_p2_ctx(a, a1, l, l1)][0]);
        let ref1 = if bit1 == 0 {
            // comp_bwdref_p1: BWDREF vs. ALTREF2, same vote as single_ref_p6.
            let p1 = dec.symbol(&mut cdfs.comp_bwdref[single_ref_p6_ctx(a, a1, l, l1)][1]);
            if p1 == 1 { ALTREF2_FRAME } else { BWDREF_FRAME }
        } else {
            ALTREF_FRAME
        };
        (ref0, ref1)
    }
}

/// Spec 5.11.24's `compound_mode` (lane-av1comp): which of the eight
/// `INTER_COMPOUND_MODES` (`NEAREST_NEARESTMV`..`NEW_NEWMV`, returned as
/// `0..=7`) a `COMPOUND_REFERENCE` block takes, reached once
/// [`read_comp_mode`] has already returned `COMPOUND_REFERENCE`. `ctx` is
/// libaom's `av1_mode_context_analyzer`'s compound branch --
/// `compound_mode_ctx_map[ref_mv_ctx >> 1][min(new_mv_ctx, COMP_NEWMV_CTXS
/// - 1)]` (`mvref_common.h`) -- folded from [`crate::mvstack::
///   find_mv_stack_compound`]'s own `new_mv_ctx`/`ref_mv_ctx`. The read symbol
///   is not yet turned into an assigned MV pair or motion-compensated --
///   lane-av1comp.
pub(super) fn read_inter_compound_mode(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    new_mv_ctx: usize,
    ref_mv_ctx: usize,
) -> u8 {
    let ctx = cdf::COMPOUND_MODE_CTX_MAP[ref_mv_ctx >> 1][new_mv_ctx.min(4)];
    hit!(COMPOUND_MODE_HITS);
    dec.symbol(&mut cdfs.inter_compound_mode[ctx]) as u8
}

/// spec 7.10.2.10 `assign_mv`'s compound branch, plus the compound half of
/// `read_drl_idx` (spec 5.11.24) it depends on -- `mode` is
/// [`read_inter_compound_mode`]'s own `0..=7` (`NEAREST_NEARESTMV`..
/// `NEW_NEWMV`, libaom `enums.h` order). Every `GLOBAL_GLOBALMV` side reads
/// as `(0, 0)` since [`crate::stream::decode_stream`] already refuses any
/// non-`IDENTITY` global motion model before a tile is ever decoded.
///
/// Ported straight from libaom `decodemv.c`'s `read_drl_idx` (the
/// `NEW_NEWMV`/`have_nearmv_in_inter_mode` branches) and `assign_mv`/its
/// caller's `ref_mv[]`/`nearmv[]` setup: the `NEAR_NEWMV`/`NEW_NEARMV`
/// `1 + ref_mv_idx` special case is folded into `new_idx` below, matching
/// `read_drl_idx`'s own comment about "offsetting the NEARESTMV mode".
#[allow(clippy::too_many_arguments)]
pub(super) fn assign_compound_mv(
    dec: &mut SymbolDecoder,
    cdfs: &mut Cdfs,
    comp_stack: &crate::mvstack::CompoundMvStack,
    mode: u8,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
    // lane-gm r2: `gm_get_motion_vector(ref0)`/`(ref1)` at this block's own
    // position, spec 7.10.2.10's `GLOBAL_GLOBALMV` assignment.
    global_mv: ((i32, i32), (i32, i32)),
    fctx: &crate::decode::FrameCtx,
) -> ((i32, i32), (i32, i32)) {
    const NEAREST_NEARESTMV: u8 = 0;
    const NEAR_NEARMV: u8 = 1;
    const NEAREST_NEWMV: u8 = 2;
    const NEW_NEARESTMV: u8 = 3;
    const NEAR_NEWMV: u8 = 4;
    const NEW_NEARMV: u8 = 5;
    const GLOBAL_GLOBALMV: u8 = 6;
    const NEW_NEWMV: u8 = 7;

    let mut ref_mv_idx = 0usize;
    if mode == NEW_NEWMV {
        let mut idx = 0usize;
        while idx < 2 && comp_stack.entries.len() > idx + 1 {
            if dec.symbol(&mut cdfs.drl_mode[comp_stack.drl_ctx[idx]]) == 0 {
                break;
            }
            idx += 1;
        }
        ref_mv_idx = idx;
    } else if matches!(mode, NEAR_NEARMV | NEAR_NEWMV | NEW_NEARMV) {
        let mut idx = 1usize;
        while idx < 3 && comp_stack.entries.len() > idx + 1 {
            if dec.symbol(&mut cdfs.drl_mode[comp_stack.drl_ctx[idx]]) == 0 {
                break;
            }
            idx += 1;
        }
        ref_mv_idx = idx - 1;
    }

    let near = comp_stack
        .entries
        .get(ref_mv_idx + 1)
        .map_or(comp_stack.near_mv, |e| (e.mv0, e.mv1));
    let new_idx = if matches!(mode, NEAR_NEWMV | NEW_NEARMV) {
        ref_mv_idx + 1
    } else {
        ref_mv_idx
    };
    let new_pred = comp_stack
        .entries
        .get(new_idx)
        .map_or(comp_stack.nearest_mv, |e| (e.mv0, e.mv1));

    let read_new = |dec: &mut SymbolDecoder, cdfs: &mut Cdfs, base: (i32, i32)| {
        read_mv(
            dec,
            &mut cdfs.mv_comp,
            &mut cdfs.mv_joint,
            base,
            allow_high_precision_mv,
            force_integer_mv,
            fctx,
        )
    };

    match mode {
        NEAREST_NEARESTMV => comp_stack.nearest_mv,
        NEAR_NEARMV => near,
        NEAREST_NEWMV => (comp_stack.nearest_mv.0, read_new(dec, cdfs, new_pred.1)),
        NEW_NEARESTMV => (read_new(dec, cdfs, new_pred.0), comp_stack.nearest_mv.1),
        NEAR_NEWMV => (near.0, read_new(dec, cdfs, new_pred.1)),
        NEW_NEARMV => (read_new(dec, cdfs, new_pred.0), near.1),
        GLOBAL_GLOBALMV => global_mv,
        _ => (
            read_new(dec, cdfs, new_pred.0),
            read_new(dec, cdfs, new_pred.1),
        ),
    }
}

/// Derives this query block's own [`crate::mvstack::GmMvTable`] -- one
/// `gm_get_motion_vector` (spec 7.10.2.1) result per reference frame, at
/// THIS block's own `(mi_row, mi_col, bw4, bh4)` (mvstack.rs's own doc: the
/// table is always computed at the *querying* block's position, never a
/// donating neighbour's).
pub(super) fn build_gm_mv_table(
    global_motion: &[ec_av1_syntax::WarpParams; 7],
    mi_row: usize,
    mi_col: usize,
    bw4: usize,
    bh4: usize,
    allow_high_precision_mv: bool,
    force_integer_mv: bool,
) -> crate::mvstack::GmMvTable {
    std::array::from_fn(|i| {
        let gm = &global_motion[i];
        crate::warp::gm_get_motion_vector(
            gm.model,
            &gm.params,
            mi_row,
            mi_col,
            bw4,
            bh4,
            allow_high_precision_mv,
            force_integer_mv,
        )
    })
}

/// Decodes one square inter-frame block (32x32 whole or 16x16 leaf): skip,
/// `is_inter`, then either the single-reference/MV-stack/motion-vector chain
/// and motion-compensated reconstruction, or the inter frame's own intra
/// path (`Y_MODE` by size group, not `KF_Y_MODE` by neighbour), mirroring
/// [`crate::tile`]'s `sb_coeff_inter_frame_tile`'s whole-block branch and its
/// private `write_inter_frame_leaf` -- both share this one function here,
/// parameterised by `side`/the transform sets/scan tables/size group they
/// each use.
///
/// # Errors
/// Returns an error when a symbol names anything this decoder does not
/// reconstruct: a reference other than `LAST_FRAME`, `GLOBALMV` (round 3;
/// `NEARESTMV`/`NEARMV`/`NEWMV` are all reconstructed), or an intra
/// mode/tx-type outside what [`read_intra_mode`]/[`read_coeffs`] already
/// refuse.
#[allow(clippy::too_many_arguments)]
/// lane-inter4 r1: 16-px-cell coordinates -> MI (4-px) coordinates, the unit
/// [`decode_inter_block`]'s `at` now takes.
pub(super) const fn sub16_to_mi(t: (usize, usize)) -> (usize, usize) {
    (t.0 * (SUB / MI), t.1 * (SUB / MI))
}

/// The luma transform leaf a chroma unit inherits its `tx_type` from
/// (libaom `av1_get_tx_type`, `blockd.h:1287-1297`): an INTER block's chroma
/// unit never codes a `tx_type` symbol of its own, so the type is read out
/// of `xd->tx_type_map` at the chroma unit's own position scaled back to luma
/// (`blk_row << ss_y`, `blk_col << ss_x`). `leaves` is the block's coded
/// transform tree in decode order, `(row, col, tw, th, tx_type)` with
/// `(row, col)` in MI relative to the block and `(tw, th)` in PIXELS;
/// `rel_mi` is the chroma unit's own luma-relative top-left MI cell. One
/// function serves the 128x128 mu-chunk tail and the 64x64 arms alike -- the
/// lookup is the same rule, so a second inline copy would be a second
/// convention.
///
/// A leaf that coded nothing resolves to `DctDct` here exactly as libaom's
/// `decodetxb.c:199-203` stamps `DCT_DCT` into that leaf's map cell on the
/// `all_zero` path: a chroma unit over a SKIPPED luma quadrant must NOT
/// inherit the block-level (top-left leaf) type.
pub(super) fn covering_leaf_tx_type(
    leaves: &[(usize, usize, usize, usize, TxType)],
    rel_mi: (usize, usize),
) -> Option<TxType> {
    leaves
        .iter()
        .copied()
        // leaf (row, col) are mi but (tw, th) are px (the rect leaf path
        // divides by MI too)
        .find(|&(lr, lc, lw, lh, _)| {
            lr <= rel_mi.0 && rel_mi.0 < lr + lh / MI && lc <= rel_mi.1 && rel_mi.1 < lc + lw / MI
        })
        .map(|(_, _, _, _, t)| t)
}

thread_local! {
    /// lane-av1ichromatx: [`read_inter_rect_chroma`] units that fell through
    /// `covering_leaf_tx_type` to the BLOCK-level `luma_tx_type`, and of those
    /// the ones whose chroma plane is MULTI-unit (`nx*ny > 1`) -- the only
    /// shape where two units of one plane can disagree, i.e. where a
    /// block-level value could actually mistype a unit.
    ///
    /// Why this needs its own number: `CHROMA_RECT_LEAF_TX_HITS` counts the
    /// units that took the covering leaf, so it says nothing about the units
    /// that did NOT -- and the fallback is the only remaining place in
    /// `decode_inter_block` where a chroma unit is handed a block-level type
    /// on a split plane.
    pub(crate) static CHROMA_RECT_BLOCK_TX_FALLBACK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

thread_local! {
    /// Of [`CHROMA_RECT_BLOCK_TX_FALLBACK`], how many were on a multi-unit
    /// chroma plane -- the non-vacuity bar for the assertion at that site.
    pub(crate) static CHROMA_RECT_BLOCK_TX_FALLBACK_MULTI: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`CHROMA_RECT_BLOCK_TX_FALLBACK`]: rect chroma units handed the block-level
/// type because no luma leaf covered their own MI cell.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_rect_block_tx_fallback() -> usize {
    CHROMA_RECT_BLOCK_TX_FALLBACK.with(std::cell::Cell::get)
}

/// [`CHROMA_RECT_BLOCK_TX_FALLBACK_MULTI`]: of those, the ones whose chroma
/// plane had more than one unit.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(crate) fn chroma_rect_block_tx_fallback_multi() -> usize {
    CHROMA_RECT_BLOCK_TX_FALLBACK_MULTI.with(std::cell::Cell::get)
}

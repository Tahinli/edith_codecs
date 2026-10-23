//! Post-filter drivers: whole-frame CDEF and loop restoration. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D20b)

use super::*;

/// `true` when this frame's CDEF cannot change a sample: `bits == 0` still
/// means "no filtering at all" whenever the single (index 0) strength pair
/// is all-zero, the fast path every existing gate stream takes.
pub(super) fn cdef_all_zero(cdef: &CdefParams) -> bool {
    cdef.bits == 0
        && cdef.y_pri_strength[0] == 0
        && cdef.y_sec_strength[0] == 0
        && cdef.uv_pri_strength[0] == 0
        && cdef.uv_sec_strength[0] == 0
}

/// [`apply_cdef`]'s band loop over the mi rows `r_lo0..r_hi0` only (the
/// whole frame when `0..mi_rows`, which is what `apply_cdef` passes).
/// `src_*` are the PRE-CDEF planes every unit reads; the writes are this
/// row range's own pixels.
#[allow(clippy::too_many_arguments)]
fn cdef_rows(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    src_y: &[u16],
    src_u: &[u16],
    src_v: &[u16],
    cdef: &CdefParams,
    skip_grid: &Neighbours,
    idx_grid: &[u8],
    sb_cols: usize,
    coeff_shift: i32,
    damping: i32,
    damping_uv: i32,
    (frame_w, frame_h): (usize, usize),
    r_lo0: usize,
    r_hi0: usize,
    fctx: &crate::decode::FrameCtx,
    // See [`cdef_band`]: the per-8x8-unit byte map, carved by unit row below.
    wrote: Option<&mut [u8]>,
) {
    let mi_rows = skip_grid.mi_rows;
    let (lo, hi) = (r_lo0.min(mi_rows), r_hi0.min(mi_rows));
    if lo >= hi {
        return;
    }
    let cbands: Vec<(usize, usize)> =
        crate::par::bands((hi - lo).div_ceil(2), crate::par::filter_threads())
            .into_iter()
            .map(|(a, b)| (lo + a * 2, (lo + b * 2).min(hi)))
            .collect();
    let geom = |p: &PlaneBuf<'_>| CdefPlaneGeom {
        stride: p.width,
        true_w: p.true_width,
        true_h: p.true_height,
    };
    let geom = [geom(y), geom(u), geom(v)];
    // Plane row a mi row starts at; chroma rows are luma rows >> ss_y.
    let shift = [0, ss_y(fctx), ss_y(fctx)];
    let row_of = |mi: usize, p: usize| (mi * 4) >> shift[p];
    let last = cbands.len() - 1;
    // Band `k` owns plane elements from its first row up to the next band's
    // first row. The last band runs to the end of the buffer, which covers
    // the padded tail a trailing odd mi row's 8x8 block writes into.
    let plane_ranges = |p: usize, len: usize| -> Vec<(usize, usize)> {
        cbands
            .iter()
            .enumerate()
            .map(|(k, &(a, b))| {
                let start = row_of(a, p) * geom[p].stride;
                let end = if k == last { len } else { row_of(b, p) * geom[p].stride };
                (start, end)
            })
            .collect()
    };
    let (yd, ud, vd) = (
        y.data.to_mut(),
        u.data.to_mut(),
        v.data.to_mut(),
    );
    let (ry, ru, rv) = (
        plane_ranges(0, yd.len()),
        plane_ranges(1, ud.len()),
        plane_ranges(2, vd.len()),
    );
    let unit_cols = skip_grid.mi_cols.div_ceil(2);
    let n = cbands.len();
    let wr: Vec<Option<&mut [u8]>> = match wrote {
        Some(w) => {
            let len = w.len();
            let ranges: Vec<(usize, usize)> = cbands
                .iter()
                .enumerate()
                .map(|(k, &(a, b))| {
                    let start = (a / 2) * unit_cols;
                    let end = if k == last { len } else { (b / 2) * unit_cols };
                    (start, end)
                })
                .collect();
            crate::par::carve_ranges(w, &ranges).into_iter().map(Some).collect()
        }
        None => (0..n).map(|_| None).collect(),
    };
    let parts: Vec<CdefBandDst<'_>> = cbands
        .iter()
        .zip(crate::par::carve_ranges(yd, &ry))
        .zip(crate::par::carve_ranges(ud, &ru))
        .zip(crate::par::carve_ranges(vd, &rv))
        .zip(wr)
        .map(|((((&(a, _), yy), uu), vv), w)| CdefBandDst {
            data: [yy, uu, vv],
            row0: [row_of(a, 0), row_of(a, 1), row_of(a, 2)],
            wrote: w,
            wrote_row0: a / 2,
        })
        .collect();
    crate::par::run_bands_with(&cbands, parts, fctx, |r_lo, r_hi, dst, fctx| {
        cdef_band(
            r_lo,
            r_hi,
            dst,
            geom,
            src_y,
            src_u,
            src_v,
            cdef,
            skip_grid,
            idx_grid,
            sb_cols,
            coeff_shift,
            damping,
            damping_uv,
            (frame_w, frame_h),
            fctx,
        );
    });
}

// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_cdef(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    cdef: &CdefParams,
    skip_grid: &Neighbours,
    // This frame header's own CROPPED luma size, for the straddling-unit
    // counter only -- CDEF itself walks the mi-rounded plane.
    (frame_w, frame_h): (usize, usize),
    fctx: &crate::decode::FrameCtx,
    // lane-av1cap: every CDEF unit reads the PRE-CDEF planes, so the filter
    // starts by copying all three (~19 MB at 4K). The encoder's filter
    // replay already holds an untouched copy of exactly those planes -- its
    // post-deblock cache, which is where the working planes were copied FROM
    // -- so it hands them straight in and the copy does not happen. `None`
    // (every decode) copies as before.
    src: Option<(&[u16], &[u16], &[u16])>,
    // See [`cdef_band`]: `Some` only from the encoder's filter replay, which
    // clears and sizes it (`mi_cols.div_ceil(2) * mi_rows.div_ceil(2)`)
    // before the call.
    wrote: Option<&mut [u8]>,
) {
    // lane-part32 r2 debug rung, env-gated: see `apply_deblock`'s sibling.
    if crate::envflags::env_flag!("EC_AV1_DEBUG_SKIP_CDEF") {
        return;
    }
    cdef_scan_counters(skip_grid);
    // `cdef.bits == 0` still means "no filtering at all" whenever the single
    // (index 0) strength pair is all-zero -- the fast path every existing
    // gate stream takes. A `bits > 0` frame can still be all-zero-strength
    // at every index, but that is rare enough not to special-case; the
    // per-superblock lookup below degrades to the same index-0 read anyway
    // when [`CDEF_IDX_GRID`] was never populated (`bits == 0`).
    if cdef_all_zero(cdef) {
        return;
    }
    let sb_cols = fctx.cdef_sb_cols.with(|c| c.get());
    // One entry per superblock (a few hundred at 4K), so this clone is not
    // one of the per-frame copies lane-perf6 pooled below.
    let idx_grid = fctx.cdef_idx_grid.with(|g| g.borrow().clone());
    let owned = src.is_none();
    let (src_y, src_u, src_v) = match src {
        Some((a, b, c)) => (
            std::borrow::Cow::Borrowed(a),
            std::borrow::Cow::Borrowed(b),
            std::borrow::Cow::Borrowed(c),
        ),
        None => (
            std::borrow::Cow::Owned(scratch_from(&y.data)),
            std::borrow::Cow::Owned(scratch_from(&u.data)),
            std::borrow::Cow::Owned(scratch_from(&v.data)),
        ),
    };
    // libaom `av1_cdef_filter_fb`: `pri_strength = level << coeff_shift`,
    // `sec_strength <<= coeff_shift`, `damping += coeff_shift - (pli != 0)`.
    let coeff_shift = i32::from(bit_depth(fctx)) - 8;
    let damping = i32::from(cdef.damping) + coeff_shift;
    let damping_uv = (damping - 1).max(0);
    // lane-filt1: every CDEF unit reads ONLY the pre-CDEF snapshots
    // (`src_y`/`src_u`/`src_v`, immutable here) and writes its own 8x8 luma /
    // 4x4 chroma square, so units are independent. Bands are mi ROW bands (2
    // mi = 8 luma rows, 4 chroma rows), which makes each band's writes a
    // disjoint row range of all three planes.
    cdef_rows(
        y,
        u,
        v,
        &src_y,
        &src_u,
        &src_v,
        cdef,
        skip_grid,
        &idx_grid,
        sb_cols,
        coeff_shift,
        damping,
        damping_uv,
        (frame_w, frame_h),
        0,
        skip_grid.mi_rows,
        fctx,
        wrote,
    );
    if owned {
        scratch_return(src_y.into_owned());
        scratch_return(src_u.into_owned());
        scratch_return(src_v.into_owned());
    }
}

/// [`FrameCtx::capture_stages`]'s write side, called between CDEF and loop
/// restoration on both tile paths: hands the encoder's restoration search the
/// two pictures spec 7.17 filters from. `deblocked` is the decoder's own
/// stripe-boundary snapshot ([`plane_snapshot`], only the rows LR reads), so
/// a capture is worth three plane copies and nothing else -- and when the
/// capture flag is off (every decode but that search) it is worth a bool.
pub(super) fn capture_stages(
    y: &PlaneBuf<'_>,
    u: &PlaneBuf<'_>,
    v: &PlaneBuf<'_>,
    deblocked: Option<&(PlaneBuf<'static>, PlaneBuf<'static>, PlaneBuf<'static>)>,
    fctx: &FrameCtx,
) {
    if !fctx.capture_stages.get() {
        return;
    }
    // Without an active restoration type the decoder took no pre-CDEF
    // snapshot; the post-CDEF plane then stands in for it, which is exact
    // everywhere but LR's own 3-row stripe borders (the encoder's search
    // always asks with a type set, so this is the defensive arm).
    let db = |i: usize, p: &PlaneBuf<'_>| match deblocked {
        Some((a, b, c)) => [a, b, c][i].data.to_vec(),
        None => p.data.to_vec(),
    };
    *fctx.stages.borrow_mut() = Some(FilterStages {
        deblocked: [db(0, y), db(1, u), db(2, v)],
        cdefed: [y.data.to_vec(), u.data.to_vec(), v.data.to_vec()],
        stride: [y.width, u.width, v.width],
    });
}

/// Spec 7.17: loop restoration, run after [`apply_cdef`] (a sibling lane's
/// ordering note: libaom also runs `superres_post_decode` between the two,
/// but this decoder never implements super-resolution, so there is nothing
/// to interleave). `deblocked_*` is each plane's post-deblock, pre-CDEF
/// buffer (the caller clones it before calling [`apply_cdef`]) -- loop
/// restoration's stripe-boundary rows read from it instead of the
/// CDEF-filtered plane, per `restoration.rs`'s own `lr_sample` doc comment.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_loop_restoration(
    y: &mut PlaneBuf<'static>,
    u: &mut PlaneBuf<'static>,
    v: &mut PlaneBuf<'static>,
    deblocked_y: &PlaneBuf<'_>,
    deblocked_u: &PlaneBuf<'_>,
    deblocked_v: &PlaneBuf<'_>,
    lr: &LoopRestorationParams,
    grid: &crate::restoration::RestorationGrid,
    // lane-golomb r8: this frame header's own (cropped) luma size. Loop
    // restoration is the ONE post-decode filter that does NOT run over the
    // mi-rounded plane: libaom's `av1_loop_restoration_filter_frame` sizes
    // every plane with `av1_get_upsampled_plane_size` -- `plane_w =
    // ROUND_POWER_OF_TWO(cm->superres_upscaled_width, ss_x)`, `plane_h =
    // ROUND_POWER_OF_TWO(cm->height, ss_y)` (`av1/common/restoration.c:47`)
    // -- and asserts it equals the buffer's CROP width
    // (`restoration.c:1106`), then `av1_extend_frame(..., plane_w, plane_h,
    // ...)` (`restoration.c:1109`) replicates the right/bottom border from
    // that cropped edge before any unit is filtered. Deblock and CDEF DO
    // walk the mi grid, which is why passing `true_width`/`true_height`
    // here went unnoticed until a frame whose size is not a multiple of 8
    // (68 -> mi-rounded 72): the LR taps at the last four real columns then
    // read this decoder's own decoded samples at columns 68..71 where
    // libaom reads column 67 replicated -- +-1 over the straddling band
    // (class av1-truesize-lane: true frame size vs mi-rounded).
    (frame_w, frame_h): (usize, usize),
    fctx: &crate::decode::FrameCtx,
) {
    // lane-svt1 debug rung, env-gated: see `apply_cdef`/`apply_deblock`'s siblings.
    if crate::envflags::env_flag!("EC_AV1_DEBUG_SKIP_LR") {
        return;
    }
    let mut out = scratch_empty();
    crate::restoration::apply_loop_restoration_plane(
        &y.data,
        &deblocked_y.data,
        y.width,
        frame_w,
        frame_h,
        0,
        lr.frame_restoration_type[0],
        lr.loop_restoration_size[0],
        grid,
        0,
        &mut out,
        None,
        fctx,
    );
    scratch_return(std::mem::replace(&mut y.data, std::borrow::Cow::Owned(out)).into_owned());
    let mut out = scratch_empty();
    crate::restoration::apply_loop_restoration_plane(
        &u.data,
        &deblocked_u.data,
        u.width,
        round_ss(frame_w, ss_x(fctx)),
        round_ss(frame_h, ss_y(fctx)),
        ss_y(fctx) as u32,
        lr.frame_restoration_type[1],
        lr.loop_restoration_size[1],
        grid,
        1,
        &mut out,
        None,
        fctx,
    );
    scratch_return(std::mem::replace(&mut u.data, std::borrow::Cow::Owned(out)).into_owned());
    let mut out = scratch_empty();
    crate::restoration::apply_loop_restoration_plane(
        &v.data,
        &deblocked_v.data,
        v.width,
        round_ss(frame_w, ss_x(fctx)),
        round_ss(frame_h, ss_y(fctx)),
        ss_y(fctx) as u32,
        lr.frame_restoration_type[2],
        lr.loop_restoration_size[2],
        grid,
        2,
        &mut out,
        None,
        fctx,
    );
    scratch_return(std::mem::replace(&mut v.data, std::borrow::Cow::Owned(out)).into_owned());
}

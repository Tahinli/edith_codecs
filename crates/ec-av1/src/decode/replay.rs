//! Filter replay and CDEF band helpers: the replay_* family that re-runs the loop filters over a captured picture, and cdef_band. Moved out of `decode/mod.rs` (av1split D11); the items are unchanged apart from visibility.

use super::*;

/// The filter half of a captured decode, under new parameters: exactly the
/// [`apply_deblock`] / [`apply_cdef`] pair the decoder's own tail runs, on a
/// copy of the captured planes. `None` when nothing is captured (the caller
/// then decodes for real).
pub(crate) fn replay_filters(
    loop_filter: &LoopFilterParams,
    cdef: &CdefParams,
    fctx: &FrameCtx,
) -> Option<Picture> {
    #[cfg(test)]
    if REPLAY_DISABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    replay_inner(loop_filter, cdef, None, fctx).map(|(picture, _)| picture)
}

/// lane-av1cap: the filter stage's LAST replay -- the WINNING parameters,
/// under the per-64x64 `cdef_idx` grid the re-coded tile carries. The stage
/// used to reach this picture by decoding that re-coded tile a second time
/// (10% of a 4K frame's encode), only because the loop-restoration search
/// needs the decoder's own post-CDEF plane
/// ([`FrameCtx::capture_stages`]). It is the same two kernel calls
/// ([`apply_deblock`], [`apply_cdef`]) on the same captured pre-filter
/// planes in the same order, so it is the same bytes -- pinned by
/// `encoder::tests::filter_replay_final_matches_the_capture_decode`, which
/// runs the decode as well and holds picture and stages against it.
///
/// `want_stages` mirrors `capture_stages`' own `deblocked = None` arm: that
/// decode's header says `RESTORE_NONE`, so the decoder took no pre-CDEF
/// snapshot and handed the post-CDEF plane over for both stages.
pub(crate) fn replay_final(
    loop_filter: &LoopFilterParams,
    cdef: &CdefParams,
    idx_grid: &[u8],
    want_stages: bool,
    fctx: &FrameCtx,
) -> Option<(Picture, Option<FilterStages>)> {
    #[cfg(test)]
    if REPLAY_DISABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    replay_inner(loop_filter, cdef, Some((idx_grid, want_stages)), fctx)
}

fn replay_inner(
    loop_filter: &LoopFilterParams,
    cdef: &CdefParams,
    // `Some((cdef_idx grid, capture the filter stages))` for the final
    // replay; `None` for a scoring candidate, which always codes `bits == 0`.
    final_pass: Option<(&[u8], bool)>,
    fctx: &FrameCtx,
) -> Option<(Picture, Option<FilterStages>)> {
    REPLAY.with_borrow_mut(|slot| {
        let r = slot.as_mut()?;
        let hit = matches!(&r.deblocked, Some((lf, ..)) if lf == loop_filter);
        // Set when the working planes ARE the deblocked planes (see below):
        // they go into the cache at the end, and the picture is cropped out
        // of them rather than taking their buffers.
        let mut share = false;
        let cpt = crate::par::timer(crate::par::S_COPY);
        let unit_cols = r.neighbours.mi_cols.div_ceil(2);
        let unit_rows = r.neighbours.mi_rows.div_ceil(2);
        let (mut y, mut u, mut v) = if hit {
            let work = r.work.take();
            let wrote = std::mem::take(&mut r.wrote);
            // `cdef_adjust_strength` is monotone in the strength it is given,
            // so a candidate whose primary strength is no lower and whose
            // secondary is zero only if the last one's was writes every unit
            // the last one wrote -- and writes it from the same post-deblock
            // source, so the samples under it need not be put back first.
            // Only while both candidates carry ONE strength pair (`bits ==
            // 0`, every scoring candidate) and read the same `cdef_idx` grid.
            let superset =
                |cur: (u8, u8), old: (u8, u8)| cur.0 >= old.0 && (cur.1 != 0 || old.1 == 0);
            let (skip_y, skip_uv) = match (final_pass, &r.work_cdef) {
                (None, Some(old)) if old.bits == 0 && cdef.bits == 0 => (
                    superset(
                        (cdef.y_pri_strength[0], cdef.y_sec_strength[0]),
                        (old.y_pri_strength[0], old.y_sec_strength[0]),
                    ),
                    superset(
                        (cdef.uv_pri_strength[0], cdef.uv_sec_strength[0]),
                        (old.uv_pri_strength[0], old.uv_sec_strength[0]),
                    ),
                ),
                _ => (false, false),
            };
            let (deblocked, scratch) = (&r.deblocked, &mut r.scratch);
            let (_, dy, du, dv) = deblocked.as_ref().expect("just matched");
            match work {
                Some((mut wy, mut wu, mut wv)) => {
                    if !skip_y {
                        restore_written(&mut wy, dy, &wrote, 1, unit_cols, 8);
                    }
                    if !skip_uv {
                        restore_written(&mut wu, du, &wrote, 2, unit_cols, 4);
                        restore_written(&mut wv, dv, &wrote, 2, unit_cols, 4);
                    }
                    (wy, wu, wv)
                }
                None => (
                    pooled(dy, scratch),
                    pooled(du, scratch),
                    pooled(dv, scratch),
                ),
            }
        } else {
            // The levels moved, so the cached deblocked planes are dead:
            // their buffers are this candidate's working copies.
            for plane in r.work.take().into_iter().flat_map(|(a, b, c)| [a, b, c]) {
                r.scratch.push(plane.data.into_owned());
            }
            r.work_cdef = None;
            if let Some((_, dy, du, dv)) = r.deblocked.take() {
                r.scratch.push(dy.data.into_owned());
                r.scratch.push(du.data.into_owned());
                r.scratch.push(dv.data.into_owned());
            }
            let (cap_y, cap_u, cap_v, scratch) = (&r.y, &r.u, &r.v, &mut r.scratch);
            let (mut y, mut u, mut v) = (
                pooled(cap_y, scratch),
                pooled(cap_u, scratch),
                pooled(cap_v, scratch),
            );
            let _t = crate::par::timer(crate::par::S_DEBLOCK);
            apply_deblock(
                &mut y,
                &mut u,
                &mut v,
                loop_filter,
                &r.neighbours,
                r.frame_width,
                r.frame_height,
                fctx,
            );
            // lane-av1cap2: an all-zero-strength CDEF writes no sample at
            // all ([`cdef_all_zero`]'s own early return), so THESE planes
            // are already this candidate's output -- they become the
            // deblock cache at the end instead of being copied into it
            // here, which is three ~6 MB copies less per candidate that
            // moves the levels (the whole deblocking half of the search:
            // its CDEF strengths are still (0,0)).
            share = cdef_all_zero(cdef);
            if !share {
                let scratch = &mut r.scratch;
                r.deblocked = Some((
                    *loop_filter,
                    pooled(&y, scratch),
                    pooled(&u, scratch),
                    pooled(&v, scratch),
                ));
            }
            (y, u, v)
        };
        drop(cpt);
        // The winner's `cdef_idx` literals live in the re-coded tile, which
        // nothing parses on this path -- hand the grid the encoder already
        // chose straight to the filter (`bits == 0` leaves the grid the last
        // parse sized, which is what every scoring candidate reads).
        if let Some((grid, _)) = final_pass.filter(|(g, _)| !g.is_empty()) {
            fctx.cdef_idx_grid.with(|g| *g.borrow_mut() = grid.to_vec());
        }
        let ct = crate::par::timer(crate::par::S_CDEF);
        let mut wrote = std::mem::take(&mut r.wrote);
        wrote.clear();
        wrote.resize(unit_cols * unit_rows, 0);
        r.work_cdef = Some(*cdef);
        let pre = r
            .deblocked
            .as_ref()
            .map(|(_, dy, du, dv)| (dy.data.as_ref(), du.data.as_ref(), dv.data.as_ref()));
        apply_cdef(
            &mut y,
            &mut u,
            &mut v,
            cdef,
            &r.neighbours,
            (r.frame_width, r.frame_height),
            fctx,
            pre,
            Some(&mut wrote),
        );
        drop(ct);
        if final_pass.is_some_and(|(_, want)| want) {
            // The loop-restoration search runs on the stages above (where
            // post-CDEF stands in for post-deblock, as the capture decode
            // had it); the FILTER this frame ends up applying reads the
            // real post-deblock rows, which is what the decoder does.
            let snapshot = if share {
                // Nothing filtered these planes after the deblock, so they
                // are the post-deblock picture themselves.
                Some(plane_snapshot(&y, 0))
            } else {
                r.deblocked
                    .as_ref()
                    .map(|(_, dy, ..)| plane_snapshot(dy, 0))
            };
            let scratch = &mut r.scratch;
            r.final_luma = snapshot.map(|d| (pooled(&y, scratch), d));
        }
        let stages = final_pass.and_then(|(_, want)| {
            want.then(|| FilterStages {
                deblocked: [y.data.to_vec(), u.data.to_vec(), v.data.to_vec()],
                cdefed: [y.data.to_vec(), u.data.to_vec(), v.data.to_vec()],
                stride: [y.width, u.width, v.width],
            })
        });
        // The capture only happens on the shape whose tail is the two
        // filter calls and the crop (no superres, no loop restoration), so
        // what is left is that crop -- byte for byte the cropped return path
        // of the decode this replaces, wide-margin stash included.
        //
        // lane-av1fpar: the crop is why this arm exists at all. The capture
        // used to demand `frame == plane` dimensions, which no editor size
        // meets (1080 and 1608 both pad to a superblock row), so every
        // candidate of every frame at those sizes fell through to a full
        // re-decode -- 57% of a 1080p frame's wall.
        let (fw, fh) = (r.frame_width, r.frame_height);
        if fw == r.width && fh == r.height {
            fctx.last_frame_wide_margin.with(|m| *m.borrow_mut() = None);
            r.wrote = Vec::new();
            r.work_cdef = None;
            if share {
                let picture = Picture {
                    width: r.width,
                    height: r.height,
                    y: y.data.to_vec(),
                    u: u.data.to_vec(),
                    v: v.data.to_vec(),
                };
                r.deblocked = Some((*loop_filter, y, u, v));
                return Some((picture, stages));
            }
            return Some((
                Picture {
                    width: r.width,
                    height: r.height,
                    y: y.data.into_owned(),
                    u: u.data.into_owned(),
                    v: v.data.into_owned(),
                },
                stages,
            ));
        }
        let crop = |plane: &PlaneBuf<'_>, w: usize, h: usize| -> Vec<u16> {
            let mut out = Vec::with_capacity(w * h);
            for row in 0..h {
                out.extend(plane.data[row * plane.width..][..w].iter().copied());
            }
            out
        };
        let (tw, th) = (r.true_width, r.true_height);
        fctx.last_frame_wide_margin.with(|m| {
            *m.borrow_mut() = (tw > fw || th > fh).then(|| Picture {
                width: tw,
                height: th,
                y: crop(&y, tw, th),
                u: crop(&u, round_ss(tw, ss_x(fctx)), round_ss(th, ss_y(fctx))),
                v: crop(&v, round_ss(tw, ss_x(fctx)), round_ss(th, ss_y(fctx))),
            });
        });
        let out = Picture {
            width: fw,
            height: fh,
            y: crop(&y, fw, fh),
            u: crop(&u, round_ss(fw, ss_x(fctx)), round_ss(fh, ss_y(fctx))),
            v: crop(&v, round_ss(fw, ss_x(fctx)), round_ss(fh, ss_y(fctx))),
        };
        // The working planes outlive the crop: either they ARE the
        // deblocked planes this candidate's successors reuse, or they go
        // back to the pool.
        if share {
            r.wrote = Vec::new();
            r.work_cdef = None;
            r.deblocked = Some((*loop_filter, y, u, v));
        } else {
            // lane-av1speed3: kept whole (post-CDEF samples and all) for the
            // next candidate, together with the map of what CDEF wrote.
            r.wrote = wrote;
            r.work = Some((y, u, v));
        }
        Some((out, stages))
    })
}

/// lane-av1cap2: the loop-restoration half of the final replay. Once the
/// units are chosen the tile is coded a second time to carry them, and that
/// re-coded tile used to be DECODED to reach the restored reference frame
/// (5.6% of a 4K frame's encode). Loop restoration reads nothing of that
/// tile but the per-unit filters the encoder just picked: it is the
/// decoder's own [`crate::restoration::apply_loop_restoration_plane`] on
/// the post-CDEF plane the final replay produced, with the TRUE
/// post-deblock stripe-border rows -- the same call the decoder's
/// `apply_loop_restoration` makes, on the same input. Only luma moves: the
/// encoder never writes a chroma restoration type other than
/// `RESTORE_NONE`, whose kernel hands its input straight back.
///
/// Returns the cropped luma plane and refreshes the wide-margin stash's own
/// luma (restoration writes strictly inside `frame_width x frame_height`,
/// so the margin columns past it stand). `None` when nothing was captured
/// -- the caller then decodes for real.
pub(crate) fn replay_restoration(
    lr: &LoopRestorationParams,
    units: &[Option<crate::restoration::WienerInfo>],
    fctx: &FrameCtx,
) -> Option<Vec<u16>> {
    #[cfg(test)]
    if REPLAY_DISABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    REPLAY.with_borrow_mut(|slot| {
        let r = slot.as_mut()?;
        let (cdefed, deblocked) = r.final_luma.as_ref()?;
        let (fw, fh) = (r.frame_width, r.frame_height);
        let mut grid = crate::restoration::RestorationGrid::with_ss(
            lr,
            fw as u32,
            fh as u32,
            fctx.subsampling_x.get(),
            fctx.subsampling_y.get(),
        );
        let (horz, vert) = (grid.horz_units[0], grid.vert_units[0]);
        if horz == 0 || units.len() != horz * vert {
            return None;
        }
        for (i, filter) in units.iter().enumerate() {
            if let Some(info) = filter {
                grid.set(
                    0,
                    i / horz,
                    i % horz,
                    crate::restoration::UnitFilter::Wiener(*info),
                );
            }
        }
        let stride = cdefed.width;
        let mut out = scratch_empty();
        crate::restoration::apply_loop_restoration_plane(
            &cdefed.data,
            &deblocked.data,
            stride,
            fw,
            fh,
            0,
            lr.frame_restoration_type[0],
            lr.loop_restoration_size[0],
            &grid,
            0,
            &mut out,
            None,
            fctx,
        );
        fctx.last_frame_wide_margin.with(|m| {
            if let Some(p) = m.borrow_mut().as_mut() {
                let tw = p.width;
                for row in 0..p.height.min(fh) {
                    let n = tw.min(fw);
                    p.y[row * tw..][..n].copy_from_slice(&out[row * stride..][..n]);
                }
            }
        });
        let mut y = Vec::with_capacity(fw * fh);
        for row in 0..fh {
            y.extend(out[row * stride..][..fw].iter().copied());
        }
        scratch_return(out);
        Some(y)
    })
}

/// The output crop, taking the plane by value: a frame whose width is
/// already superblock-aligned (only its height is padded) has a padded
/// buffer whose stride equals the visible width, so the crop is a pure
/// truncation of the buffer we already own -- no full-plane copy at all
/// (lane-copy: three ~6 MB memmoves per 4K frame on the frame thread's
/// serial tail). Any other shape falls back to the row-by-row copy.
pub(super) fn crop_owned(plane: PlaneBuf<'_>, w: usize, h: usize) -> Vec<u16> {
    let stride = plane.width;
    let mut data = plane.data.into_owned();
    if stride == w {
        data.truncate(w * h);
        return data;
    }
    let mut out = Vec::with_capacity(w * h);
    for row in 0..h {
        out.extend(data[row * stride..][..w].iter().copied());
    }
    out
}

/// Hands a buffer back to the pool; the capacity is what is worth keeping.
/// Over [`SCRATCH_CAP`] the buffer is dropped (freed) instead -- the cap is
/// what keeps a pool shared by every thread from being an unbounded stash.
pub(super) fn scratch_return(v: Vec<u16>) {
    let mut p = PLANE_SCRATCH.lock().unwrap();
    if p.len() < SCRATCH_CAP {
        p.push(v);
    }
}

/// The geometry one CDEF plane needs: its row stride and the frame's true
/// (decodable) extent in that plane's own units.
#[derive(Clone, Copy)]
pub(super) struct CdefPlaneGeom {
    pub(super) stride: usize,
    pub(super) true_w: usize,
    pub(super) true_h: usize,
}

/// One CDEF band's writable state: each plane's own row slice (the rows the
/// band's mi rows cover, carved disjointly by `cdef_rows`), the plane row
/// that slice starts at, and the band's slice of the `wrote` map with the
/// unit row it starts at. A band writes nothing outside these slices.
pub(super) struct CdefBandDst<'a> {
    pub(super) data: [&'a mut [u16]; 3],
    pub(super) row0: [usize; 3],
    // lane-av1speed3: one byte per 8x8 CDEF unit, set for every unit this band
    // actually filters. Bit 0 = this unit's LUMA square was filtered, bit 1 =
    // its chroma ones. `cdef_adjust_strength` zeroes a unit's primary strength
    // off its own variance, so which units a candidate writes is not a function
    // of its strengths alone -- the encoder's filter replay reuses one
    // destination plane across candidates and needs the map to restore it.
    pub(super) wrote: Option<&'a mut [u8]>,
    pub(super) wrote_row0: usize,
}

/// One CDEF mi-row band. The band owns its rows of the destination planes
/// through `dst` (no shared `&mut` of the whole plane), and reads only the
/// PRE-CDEF snapshots `src_*`, indexed with absolute coordinates. Reference
/// parameters restore `noalias` (lane-perf11 (a): -x% instructions at one
/// filter thread).
#[allow(clippy::too_many_arguments)]
pub(super) fn cdef_band(
    r_lo: usize,
    r_hi: usize,
    mut dst: CdefBandDst<'_>,
    geom: [CdefPlaneGeom; 3],
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
    fctx: &crate::decode::FrameCtx,
) {
    let unit_cols = skip_grid.mi_cols.div_ceil(2);
    let strength_idx = |mi_r: usize, mi_c: usize| -> usize {
        if sb_cols == 0 {
            return 0;
        }
        let (sb_r, sb_c) = (mi_r / SB_MI as usize, mi_c / SB_MI as usize);
        idx_grid.get(sb_r * sb_cols + sb_c).copied().unwrap_or(0) as usize
    };
    // One scratch window per plane kind, reused by every unit (8x8 luma and
    // 4x4 chroma, each grown by the [`CDEF_BORDER`] tap ring).
    // 256 entries, not the (bw + 4) x (bh + 4) samples themselves: the
    // AVX2 kernel taps the window in place with two-row loads that reach
    // past the last sample, and the tail stays zero for every unit.
    let mut win_y = [0i16; 256];
    let mut win_uv = [0i16; 256];
    let mut mi_r = r_lo;
    while mi_r < r_hi {
        let mut mi_c = 0usize;
        while mi_c < skip_grid.mi_cols {
            if skip_grid.is_skip_txfm(mi_r, mi_c) {
                hit!(CDEF_SKIPPED_UNITS);
            }
            if !skip_grid.is_skip_txfm(mi_r, mi_c) {
                let sidx = strength_idx(mi_r, mi_c);
                let (ox, oy) = (mi_c * 4, mi_r * 4);
                // Nothing to do for this unit at all: the direction search is
                // only ever consumed through a strength, so a unit whose four
                // strengths are zero never needs its 8x8 window gathered.
                if cdef.y_pri_strength[sidx] == 0
                    && cdef.y_sec_strength[sidx] == 0
                    && cdef.uv_pri_strength[sidx] == 0
                    && cdef.uv_sec_strength[sidx] == 0
                {
                    mi_c += 2;
                    continue;
                }
                let g = geom[0];
                cdef_gather(
                    src_y, g.stride, g.true_w, g.true_h, ox, oy, 8, 8, &mut win_y,
                );
                // lane-cdef r1: the direction search reads the SAME padded
                // block the taps do -- libaom runs `cdef_find_dir` on the
                // prepared filter block (`cdef_block.c:296-320` over
                // `cdef_prepare_fb`'s buffer, `cdef.c:249-256`), whose columns
                // and rows past the mi-rounded frame extent are CDEF_VERY_LARGE.
                // Clamping to the last real sample instead made every straddling
                // edge 8x8 (odd `mi_cols`/`mi_rows`) pick a different var, hence
                // a different `adjust_strength`, hence a +-1 band at the crop edge.
                let (dir, var) = cdef_find_dir(&win_y, 8 + 2 * CDEF_BORDER, coeff_shift);

                // libaom `av1_cdef_filter_fb` passes `pri_strength ? dir : 0`
                // -- each plane zeroes the shared direction when *its own*
                // frame-level primary strength is 0, independent of the
                // per-block adjusted `t` and of the other plane's strength.
                let y_pri_strength = i32::from(cdef.y_pri_strength[sidx]) << coeff_shift;
                let y_sec_strength = i32::from(cdef.y_sec_strength[sidx]) << coeff_shift;
                let y_dir = if y_pri_strength != 0 { dir } else { 0 };
                let t = cdef_adjust_strength(y_pri_strength, var);
                // lane-cdef r1 rung: one line per filtered 8x8 luma CDEF unit.
                if crate::envflags::env_flag!("EC_AV1_CDEF_DBG") {
                    eprintln!(
                        "EC_CDEF mi_r={mi_r} mi_c={mi_c} x={ox} y={oy} sidx={sidx} dir={dir} var={var} t={t} pri={y_pri_strength} sec={y_sec_strength} damp={damping}"
                    );
                }
                let enable_primary = t != 0;
                let enable_secondary = y_sec_strength != 0;
                if (ox + 8 > frame_w || oy + 8 > frame_h)
                    && (enable_primary
                        || enable_secondary
                        || cdef.uv_pri_strength[sidx] != 0
                        || cdef.uv_sec_strength[sidx] != 0)
                {
                    hit!(CDEF_STRADDLE_UNITS);
                }
                if enable_primary || enable_secondary {
                    cdef_filter_block(
                        &win_y,
                        &mut *dst.data[0],
                        g.stride,
                        ox,
                        oy - dst.row0[0],
                        8,
                        8,
                        t,
                        y_sec_strength,
                        y_dir,
                        damping,
                        damping,
                        enable_primary,
                        enable_secondary,
                        coeff_shift,
                        fctx,
                    );
                }

                let t_uv = i32::from(cdef.uv_pri_strength[sidx]) << coeff_shift;
                let uv_sec_strength = i32::from(cdef.uv_sec_strength[sidx]) << coeff_shift;
                // lane-av1422filter: the chroma direction is the luma one
                // remapped through `conv422`/`conv440` under asymmetric
                // subsampling (see [`CDEF_DIR_CONV422`]); identical to the
                // raw luma direction for 4:2:0 and 4:4:4.
                let uv_dir = if t_uv != 0 {
                    match (ss_x(fctx), ss_y(fctx)) {
                        (1, 0) => CDEF_DIR_CONV422[dir],
                        (0, 1) => CDEF_DIR_CONV440[dir],
                        _ => dir,
                    }
                } else {
                    0
                };
                let enable_primary_uv = t_uv != 0;
                let enable_secondary_uv = uv_sec_strength != 0;
                if let Some(w) = dst.wrote.as_deref_mut() {
                    let mark = u8::from(enable_primary || enable_secondary)
                        | (u8::from(enable_primary_uv || enable_secondary_uv) << 1);
                    w[(mi_r / 2 - dst.wrote_row0) * unit_cols + mi_c / 2] = mark;
                }
                if enable_primary_uv || enable_secondary_uv {
                    let (cox, coy) = (ox >> ss_x(fctx), oy >> ss_y(fctx));
                    let (csize_x, csize_y) = (8 >> ss_x(fctx), 8 >> ss_y(fctx));
                    for (p, src_p) in [(1usize, src_u), (2, src_v)] {
                        let gp = geom[p];
                        cdef_gather(
                            src_p,
                            gp.stride,
                            gp.true_w,
                            gp.true_h,
                            cox,
                            coy,
                            csize_x,
                            csize_y,
                            &mut win_uv,
                        );
                        cdef_filter_block(
                            &win_uv,
                            &mut *dst.data[p],
                            gp.stride,
                            cox,
                            coy - dst.row0[p],
                            csize_x,
                            csize_y,
                            t_uv,
                            uv_sec_strength,
                            uv_dir,
                            damping_uv,
                            damping_uv,
                            enable_primary_uv,
                            enable_secondary_uv,
                            coeff_shift,
                            fctx,
                        );
                    }
                }
            }
            mi_c += 2;
        }
        mi_r += 2;
    }
}

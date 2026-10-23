//! Deblocking filters: the edge filters, the deblock plane drivers, the filter-replay capture and the pooled plane scratch. Moved out of `decode/mod.rs` (av1split D12); the items are unchanged apart from visibility.

use super::*;

/// `update_sharpness`'s per-level `(blimit, limit, hev_thr)` triple.
fn deblock_thresholds(level: i32, sharpness: u8) -> (i32, i32, i32) {
    let sharpness = i32::from(sharpness);
    let shift = i32::from(sharpness > 0) + i32::from(sharpness > 4);
    let mut lim = level >> shift;
    if sharpness > 0 {
        lim = lim.min(9 - sharpness);
    }
    lim = lim.max(1);
    (2 * (level + 2) + lim, lim, level >> 4)
}

#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(super) fn sclamp(v: i32, fctx: &crate::decode::FrameCtx) -> i32 {
    // libaom's `signed_char_clamp_high`: the clamp range scales with
    // `128 << (bit_depth - 8)` alongside the centering constant below.
    let scale = 1i32 << (bit_depth(fctx) - 8);
    v.clamp(-128 * scale, 128 * scale - 1)
}

fn round_pow2(v: i32, n: u32) -> i32 {
    (v + (1 << (n - 1))) >> n
}

/// `filter4`: the narrow 4-tap kernel, and every wider kernel's non-flat
/// fallback.
pub(super) fn filter4(
    mask: bool,
    thresh: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    fctx: &crate::decode::FrameCtx,
) -> [i32; 4] {
    if !mask {
        return [p1, p0, q0, q1];
    }
    let hev = (p1 - p0).abs() > thresh || (q1 - q0).abs() > thresh;
    // lane-perf6: `sclamp` re-read the thread-local bit depth on every one of
    // this kernel's eight clamps, and this kernel runs per deblocked sample.
    // Same bound, resolved once.
    let scale = 1i32 << (bit_depth(fctx) - 8);
    let sclamp = |v: i32| v.clamp(-128 * scale, 128 * scale - 1);
    let centre = 128i32 * scale;
    let (ps1, ps0, qs0, qs1) = (p1 - centre, p0 - centre, q0 - centre, q1 - centre);
    let outer = if hev { sclamp(ps1 - qs1) } else { 0 };
    let filter = sclamp(outer + 3 * (qs0 - ps0));
    let filter1 = sclamp(filter + 4) >> 3;
    let filter2 = sclamp(filter + 3) >> 3;
    let oq0 = sclamp(qs0 - filter1) + centre;
    let op0 = sclamp(ps0 + filter2) + centre;
    let tap = if hev { 0 } else { round_pow2(filter1, 1) };
    let oq1 = sclamp(qs1 - tap) + centre;
    let op1 = sclamp(ps1 + tap) + centre;
    [op1, op0, oq0, oq1]
}

/// `filter6`: chroma's flat 5-tap smoothing (spec `[1,2,2,2,1]`) over
/// `filter4`'s narrow taps.
#[allow(clippy::too_many_arguments)]
pub(super) fn filter6(
    mask: bool,
    thresh: i32,
    flat: bool,
    p2: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    fctx: &crate::decode::FrameCtx,
) -> [i32; 4] {
    if flat && mask {
        [
            round_pow2(p2 * 3 + p1 * 2 + p0 * 2 + q0, 3),
            round_pow2(p2 + p1 * 2 + p0 * 2 + q0 * 2 + q1, 3),
            round_pow2(p1 + p0 * 2 + q0 * 2 + q1 * 2 + q2, 3),
            round_pow2(p0 + q0 * 2 + q1 * 2 + q2 * 3, 3),
        ]
    } else {
        filter4(mask, thresh, p1, p0, q0, q1, fctx)
    }
}

/// `filter8`: luma's flat 7-tap smoothing (spec `[1,1,1,2,1,1,1]`).
#[allow(clippy::too_many_arguments)]
pub(super) fn filter8(
    mask: bool,
    thresh: i32,
    flat: bool,
    p3: i32,
    p2: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    q3: i32,
    fctx: &crate::decode::FrameCtx,
) -> [i32; 6] {
    if flat && mask {
        [
            round_pow2(p3 + p3 + p3 + 2 * p2 + p1 + p0 + q0, 3),
            round_pow2(p3 + p3 + p2 + 2 * p1 + p0 + q0 + q1, 3),
            round_pow2(p3 + p2 + p1 + 2 * p0 + q0 + q1 + q2, 3),
            round_pow2(p2 + p1 + p0 + 2 * q0 + q1 + q2 + q3, 3),
            round_pow2(p1 + p0 + q0 + 2 * q1 + q2 + q3 + q3, 3),
            round_pow2(p0 + q0 + q1 + 2 * q2 + q3 + q3 + q3, 3),
        ]
    } else {
        let [op1, op0, oq0, oq1] = filter4(mask, thresh, p1, p0, q0, q1, fctx);
        [p2, op1, op0, oq0, oq1, q2]
    }
}

/// `filter14`: the wide 13-tap smoothing used at 16x16-and-larger transform
/// edges; falls back to [`filter8`] (leaving `op6`/`oq6` untouched either
/// way -- they are only ever taps here, never written).
#[allow(clippy::too_many_arguments)]
pub(super) fn filter14(
    mask: bool,
    thresh: i32,
    flat: bool,
    flat2: bool,
    p6: i32,
    p5: i32,
    p4: i32,
    p3: i32,
    p2: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    q3: i32,
    q4: i32,
    q5: i32,
    q6: i32,
    fctx: &crate::decode::FrameCtx,
) -> [i32; 12] {
    if flat2 && flat && mask {
        [
            round_pow2(p6 * 7 + p5 * 2 + p4 * 2 + p3 + p2 + p1 + p0 + q0, 4),
            round_pow2(
                p6 * 5 + p5 * 2 + p4 * 2 + p3 * 2 + p2 + p1 + p0 + q0 + q1,
                4,
            ),
            round_pow2(
                p6 * 4 + p5 + p4 * 2 + p3 * 2 + p2 * 2 + p1 + p0 + q0 + q1 + q2,
                4,
            ),
            round_pow2(
                p6 * 3 + p5 + p4 + p3 * 2 + p2 * 2 + p1 * 2 + p0 + q0 + q1 + q2 + q3,
                4,
            ),
            round_pow2(
                p6 * 2 + p5 + p4 + p3 + p2 * 2 + p1 * 2 + p0 * 2 + q0 + q1 + q2 + q3 + q4,
                4,
            ),
            round_pow2(
                p6 + p5 + p4 + p3 + p2 + p1 * 2 + p0 * 2 + q0 * 2 + q1 + q2 + q3 + q4 + q5,
                4,
            ),
            round_pow2(
                p5 + p4 + p3 + p2 + p1 + p0 * 2 + q0 * 2 + q1 * 2 + q2 + q3 + q4 + q5 + q6,
                4,
            ),
            round_pow2(
                p4 + p3 + p2 + p1 + p0 + q0 * 2 + q1 * 2 + q2 * 2 + q3 + q4 + q5 + q6 * 2,
                4,
            ),
            round_pow2(
                p3 + p2 + p1 + p0 + q0 + q1 * 2 + q2 * 2 + q3 * 2 + q4 + q5 + q6 * 3,
                4,
            ),
            round_pow2(
                p2 + p1 + p0 + q0 + q1 + q2 * 2 + q3 * 2 + q4 * 2 + q5 + q6 * 4,
                4,
            ),
            round_pow2(
                p1 + p0 + q0 + q1 + q2 + q3 * 2 + q4 * 2 + q5 * 2 + q6 * 5,
                4,
            ),
            round_pow2(p0 + q0 + q1 + q2 + q3 + q4 * 2 + q5 * 2 + q6 * 7, 4),
        ]
    } else {
        let [op2, op1, op0, oq0, oq1, oq2] =
            filter8(mask, thresh, flat, p3, p2, p1, p0, q0, q1, q2, q3, fctx);
        [p5, p4, p3, op2, op1, op0, oq0, oq1, oq2, q3, q4, q5]
    }
}

/// Filters one 4-pixel edge group: `base` is the plane-local byte offset of
/// the first (`q0`) sample past the edge, `outer_stride` advances to the
/// next of the 4 perpendicular samples, `tap_stride` steps from one tap to
/// the next along the edge's own filtering direction.
#[allow(clippy::too_many_arguments)]
pub(super) fn filter_edge(
    data: &mut [u16],
    base: usize,
    outer_stride: isize,
    tap_stride: isize,
    len: u8,
    level: i32,
    sharpness: u8,
    fctx: &crate::decode::FrameCtx,
) {
    let (blimit, limit, hev_thr) = deblock_thresholds(level, sharpness);
    // libaom `highbd_filter_mask*`/`highbd_flat_mask4`/`highbd_hev_mask`:
    // every 8-bit threshold (including the flat masks' fixed `1`) is
    // compared against a raw pixel difference on the stream's own bit-depth
    // scale, so each is shifted left by `bit_depth - 8` before use.
    let shift = bit_depth(fctx) - 8;
    let (blimit, limit, hev_thr) = (blimit << shift, limit << shift, hev_thr << shift);
    let flat_thresh = 1i32 << shift;
    let smax = sample_max(fctx);
    for i in 0..4isize {
        let center = (base as isize + i * outer_stride) as usize;
        let idx = |k: isize| -> usize { (center as isize + k * tap_stride) as usize };
        let px = |k: isize| -> i32 { i32::from(data[idx(k)]) };
        match len {
            4 => {
                let (p1, p0, q0, q1) = (px(-2), px(-1), px(0), px(1));
                let mask = (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let [op1, op0, oq0, oq1] = filter4(mask, hev_thr, p1, p0, q0, q1, fctx);
                data[idx(-2)] = op1.clamp(0, smax) as u16;
                data[idx(-1)] = op0.clamp(0, smax) as u16;
                data[idx(0)] = oq0.clamp(0, smax) as u16;
                data[idx(1)] = oq1.clamp(0, smax) as u16;
            }
            6 => {
                let (p2, p1, p0, q0, q1, q2) = (px(-3), px(-2), px(-1), px(0), px(1), px(2));
                let mask = (p2 - p1).abs() <= limit
                    && (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (q2 - q1).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let flat = (p1 - p0).abs() <= flat_thresh
                    && (q1 - q0).abs() <= flat_thresh
                    && (p2 - p0).abs() <= flat_thresh
                    && (q2 - q0).abs() <= flat_thresh;
                let [op1, op0, oq0, oq1] =
                    filter6(mask, hev_thr, flat, p2, p1, p0, q0, q1, q2, fctx);
                data[idx(-2)] = op1.clamp(0, smax) as u16;
                data[idx(-1)] = op0.clamp(0, smax) as u16;
                data[idx(0)] = oq0.clamp(0, smax) as u16;
                data[idx(1)] = oq1.clamp(0, smax) as u16;
            }
            8 => {
                let (p3, p2, p1, p0, q0, q1, q2, q3) =
                    (px(-4), px(-3), px(-2), px(-1), px(0), px(1), px(2), px(3));
                let mask = (p3 - p2).abs() <= limit
                    && (p2 - p1).abs() <= limit
                    && (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (q2 - q1).abs() <= limit
                    && (q3 - q2).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let flat = (p1 - p0).abs() <= flat_thresh
                    && (q1 - q0).abs() <= flat_thresh
                    && (p2 - p0).abs() <= flat_thresh
                    && (q2 - q0).abs() <= flat_thresh
                    && (p3 - p0).abs() <= flat_thresh
                    && (q3 - q0).abs() <= flat_thresh;
                let [op2, op1, op0, oq0, oq1, oq2] =
                    filter8(mask, hev_thr, flat, p3, p2, p1, p0, q0, q1, q2, q3, fctx);
                data[idx(-3)] = op2.clamp(0, smax) as u16;
                data[idx(-2)] = op1.clamp(0, smax) as u16;
                data[idx(-1)] = op0.clamp(0, smax) as u16;
                data[idx(0)] = oq0.clamp(0, smax) as u16;
                data[idx(1)] = oq1.clamp(0, smax) as u16;
                data[idx(2)] = oq2.clamp(0, smax) as u16;
            }
            _ => {
                let (p6, p5, p4, p3, p2, p1, p0) =
                    (px(-7), px(-6), px(-5), px(-4), px(-3), px(-2), px(-1));
                let (q0, q1, q2, q3, q4, q5, q6) =
                    (px(0), px(1), px(2), px(3), px(4), px(5), px(6));
                let mask = (p3 - p2).abs() <= limit
                    && (p2 - p1).abs() <= limit
                    && (p1 - p0).abs() <= limit
                    && (q1 - q0).abs() <= limit
                    && (q2 - q1).abs() <= limit
                    && (q3 - q2).abs() <= limit
                    && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= blimit;
                let flat = (p1 - p0).abs() <= flat_thresh
                    && (q1 - q0).abs() <= flat_thresh
                    && (p2 - p0).abs() <= flat_thresh
                    && (q2 - q0).abs() <= flat_thresh
                    && (p3 - p0).abs() <= flat_thresh
                    && (q3 - q0).abs() <= flat_thresh;
                let flat2 = (p4 - p0).abs() <= flat_thresh
                    && (q4 - q0).abs() <= flat_thresh
                    && (p5 - p0).abs() <= flat_thresh
                    && (q5 - q0).abs() <= flat_thresh
                    && (p6 - p0).abs() <= flat_thresh
                    && (q6 - q0).abs() <= flat_thresh;
                let out = filter14(
                    mask, hev_thr, flat, flat2, p6, p5, p4, p3, p2, p1, p0, q0, q1, q2, q3, q4, q5,
                    q6, fctx,
                );
                for (k, v) in (-6..=5isize).zip(out) {
                    data[idx(k)] = v.clamp(0, smax) as u16;
                }
            }
        }
    }
}

/// lane-perf8: [`filter_edge`]'s AVX2 kernel -- the scalar loop was 8.0% of a
/// 4K decode's self time, spent almost entirely on the per-tap strided
/// addressing (`perf annotate`'s hot lines are `leaq`/`movzwl` pairs, not the
/// arithmetic). Nothing here is spec logic: the same masks, the same
/// `filter4`/`filter6`/`filter8`/`filter14` weighted sums and the same
/// rounding, done for 16 edge lines at once.
///
/// 16-bit lanes are exact for every value these filters see. Samples are in
/// `[0, 4095]`, so a difference and the `blimit` term (`2 * |p0 - q0| +
/// |p1 - q1| / 2 <= 10237`) fit `i16`; `filter4` works in the centred domain
/// where every intermediate is clamped to `+-128 << (bit_depth - 8)`; and the
/// wide kernels' weights each total 8 or 16, so their sums (at most
/// `16 * 4095 + 8 = 65528`) fit `u16` -- hence `_mm256_srli_epi16` for their
/// rounding shift, which is the unsigned shift the scalar's non-negative sum
/// takes.
/// [`filter_edge`] for 16 adjacent edge lines that share `edge_params`'
/// output. Returns `false` when this build or CPU has no kernel, leaving the
/// caller on the scalar path.
#[allow(unused_variables)]
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
#[multiversion::multiversion(targets("x86_64+avx2"))]
pub(super) fn filter_edge16(
    data: &mut [u16],
    base: usize,
    stride: usize,
    vertical: bool,
    len: u8,
    level: i32,
    sharpness: u8,
    fctx: &crate::decode::FrameCtx,
) -> bool {
    multiversion::target::match_target! {
        "x86_64+avx2" => {
            let (blimit, limit, hev_thr) = deblock_thresholds(level, sharpness);
            let shift = bit_depth(fctx) - 8;
            deblock_simd::edge16(
                data,
                base,
                stride,
                vertical,
                len,
                blimit << shift,
                limit << shift,
                hev_thr << shift,
                1 << shift,
                1 << shift,
                sample_max(fctx),
            );
            true
        }
        _ => false,
    }
}

/// One plane's worth of spec 7.14: every vertical edge across the plane,
/// then every horizontal edge (the order the spec and libaom both use).
pub(super) fn deblock_plane(
    plane: &mut PlaneBuf<'static>,
    plane_idx: usize,
    lf: &LoopFilterParams,
    n: &Neighbours,
    frame_width: usize,
    frame_height: usize,
    fctx: &crate::decode::FrameCtx,
) {
    // spec 7.14's order: this plane's whole vertical pass, then its whole
    // horizontal one. `0..usize::MAX` is the whole plane, i.e. exactly this call.
    deblock_plane_span(
        plane,
        plane_idx,
        lf,
        n,
        frame_width,
        frame_height,
        0,
        usize::MAX,
        false,
        fctx,
    );
    deblock_plane_span(
        plane,
        plane_idx,
        lf,
        n,
        frame_width,
        frame_height,
        0,
        usize::MAX,
        true,
        fctx,
    );
}

/// One pass of [`deblock_plane`] -- vertical edges (`horz == false`) or
/// horizontal ones -- restricted to the plane rows `span_lo..span_hi`.
///
/// A row span is a legal cut of either pass because a band boundary is a
/// multiple of 16 rows and every edge's kernel stays inside its own
/// 4-sample group's reach.
#[allow(clippy::too_many_arguments)]
pub(super) fn deblock_plane_span(
    plane: &mut PlaneBuf<'static>,
    plane_idx: usize,
    lf: &LoopFilterParams,
    n: &Neighbours,
    frame_width: usize,
    frame_height: usize,
    span_lo: usize,
    span_hi: usize,
    horz: bool,
    fctx: &crate::decode::FrameCtx,
) {
    let chroma = plane_idx != 0;
    // r7: libaom clips the deblock loop to the CODED frame's own
    // width/height, not the mi-aligned `true_width`/`true_height` margin
    // (the padding up to the next 8-pixel/superblock boundary that
    // reconstruction still fills in but the loop filter never visits).
    // Confirmed against the superres gate: with the margin included, the
    // last coded mi column's horizontal edges were filtered when libaom's
    // real output leaves them untouched (r6's post-deblock diff, 37 px, all
    // in columns 44-47); clipping here closes it (3/3 frames pixel-exact).
    // lane-golomb r9: the bound is the MI-ROUNDED frame, not the coded
    // frame. libaom's `av1_filter_block_plane_vert/horz` walk the superblock
    // and stop at `cm->mi_params.mi_cols`/`mi_rows`, which
    // `av1_set_mb_mi` derives from `ALIGN_POWER_OF_TWO(width, 3)` -- the
    // 8-pixel-aligned frame, i.e. a 68-wide frame is filtered out to column
    // 72 and the vertical edge AT x=68 is a real edge that rewrites the
    // in-crop columns 64..67. Clipping at `frame_width` skipped it, which is
    // the +-1 straddling-band defect (68x192 cq35, frame 1, 141 luma pixels
    // all in columns 64..67; frame 0 survived because that edge carried no
    // filter level there). r7's superres measurement is preserved: its frame
    // was already 8-aligned, so this bound equals `frame_width` there and
    // still excludes the superblock padding up to `true_width`.
    // lane-av1-444: the chroma crop follows the sequence subsampling, not a
    // hardcoded 4:2:0 halving -- at 4:4:4 the halved bound clipped the whole
    // filter to the top-left quarter of each chroma plane.
    let (cw, ch) = if chroma {
        (
            frame_width.div_ceil(1 << ss_x(fctx)),
            frame_height.div_ceil(1 << ss_y(fctx)),
        )
    } else {
        (frame_width, frame_height)
    };
    let (tw, th, stride) = (
        plane.true_width.min(cw),
        plane.true_height.min(ch),
        plane.width,
    );
    if crate::envflags::env_flag!("EC_AV1_DEBLOCK_TRACE_V") {
        eprintln!(
            "DEBLOCK plane={plane_idx} tw={tw} th={th} stride={stride} true={}x{} crop={cw}x{ch}",
            plane.true_width, plane.true_height
        );
    }
    // lane-perf8: both passes iterate the edge's OWN direction innermost, so
    // 4 consecutive 4-sample groups (16 lines) can go through one AVX2
    // kernel. The swap is order-preserving: a filtered group overlaps only
    // groups along the edge it sits on -- the vertical pass's writes at
    // `x0` reach columns `x0 - 7 ..= x0 + 5` of its own 4 rows, so the
    // dependence is on ascending `x0` within a row, which stays the outer
    // loop's order, and rows remain independent (horizontal pass mirrored).
    let rows = plane.data.len() / stride.max(1);
    let seg_lf_active = fctx.seg.with(|s| s.borrow().enabled);
    // lane-filt1: both passes are split across `EC_AV1_FILTER_THREADS`
    // bands along the axis whose writes are disjoint. A vertical edge's
    // 4-sample groups write columns `x0-7 ..= x0+5` of their OWN 4 rows, so
    // row bands never touch each other; a horizontal edge's groups write rows
    // `y0-7 ..= y0+6` of their own 4 columns, so column bands never do. Both
    // read the mode-info grids immutably. The grouping a band starts with is
    // free: every 4-row (4-column) group's output is a pure function of that
    // group, whether it goes through `filter_edge16` with three neighbours or
    // through `filter_edge` alone. Band boundaries are multiples of 16 so no
    // group ever straddles one.
    // lane-perf10 merge: the output plane is owned (perf10 made mutable
    // plane params PlaneBuf<'static>); force the Cow to Owned ONCE here so
    // the bands below never clone it concurrently.
    let data: &mut [u16] = plane.data.to_mut();
    let threads = crate::par::filter_threads();
    let (sp_lo, sp_hi) = (span_lo.min(th), span_hi.min(th));
    let mut vbands: Vec<(usize, usize)> = if horz || sp_lo >= sp_hi {
        Vec::new()
    } else {
        crate::par::bands((sp_hi - sp_lo).div_ceil(16), threads)
            .into_iter()
            .map(|(a, b)| (sp_lo + a * 16, (sp_lo + b * 16).min(sp_hi)))
            .collect()
    };
    // The last band's slice runs to the end of the plane: an edge group that
    // starts inside the band can write up to 3 rows past `sp_hi` (the loop
    // below still stops at `sp_hi`), as it always has.
    if let Some(last) = vbands.last_mut() {
        last.1 = rows.max(last.1);
    }
    crate::par::run_bands_mut(&vbands, stride, fctx, data, |y_lo, y_hi, mine, fctx| {
        let y_hi = y_hi.min(sp_hi);
        // lane-deblock2: this band's own level table and pass constants,
        // built once per band instead of re-derived per 4-pixel edge.
        // `mine` holds rows `y_lo..`, so every index below is `y - y_lo` rows in.
        let vctx = EdgeCtx::new(lf, n, plane_idx, 0, chroma, seg_lf_active, fctx);
        let mut x0 = 4usize;
        while x0 < tw {
            let mut y0 = y_lo;
            while y0 < y_hi {
                if y0 + 16 <= y_hi && x0 >= 8 && x0 + 8 <= stride {
                    let ps: [Option<(u8, i32)>; 4] =
                        std::array::from_fn(|k| edge_params(&vctx, x0, y0 + 4 * k));
                    if let Some((len, level)) = ps[0]
                        && ps.iter().all(|p| *p == ps[0])
                            && filter_edge16(
                                mine,
                                (y0 - y_lo) * stride + x0,
                                stride,
                                true,
                                len,
                                level,
                                lf.sharpness,
                                fctx,
                            )
                        {
                            y0 += 16;
                            continue;
                        }
                    for (k, p) in ps.iter().enumerate() {
                        if let Some((len, level)) = *p {
                            filter_edge(
                                mine,
                                (y0 + 4 * k - y_lo) * stride + x0,
                                stride as isize,
                                1,
                                len,
                                level,
                                lf.sharpness,
                                fctx,
                            );
                        }
                    }
                    y0 += 16;
                    continue;
                }
                if let Some((len, level)) = edge_params(&vctx, x0, y0) {
                    if crate::envflags::env_flag!("EC_AV1_DEBLOCK_TRACE_V") && plane_idx == 0 {
                        eprintln!("VEDGE x0={x0} y0={y0} len={len} level={level}");
                    }
                    filter_edge(
                        mine,
                        (y0 - y_lo) * stride + x0,
                        stride as isize,
                        1,
                        len,
                        level,
                        lf.sharpness,
                        fctx,
                    );
                }
                y0 += 4;
            }
            x0 += 4;
        }
    });
    // The horizontal pass runs only after the WHOLE vertical pass of this
    // plane (spec 7.14 order), which `run_bands_mut` guarantees by joining.
    let hbands: Vec<(usize, usize)> = if !horz || sp_lo >= sp_hi {
        Vec::new()
    } else {
        crate::par::bands(tw.div_ceil(16), threads)
            .into_iter()
            .map(|(a, b)| (a * 16, (b * 16).min(tw)))
            .collect()
    };
    // A column band is not a contiguous run of the row-major plane, so each
    // band filters a private strip holding its own columns of every row,
    // copied in from the (still unchanged) plane and written back after the
    // join. A horizontal edge's taps run down its own column only, so the
    // strip has everything the band reads and writes.
    struct HStrip {
        x_lo: usize,
        x_hi: usize,
        w: usize,
        buf: Vec<u16>,
    }
    let nb = hbands.len();
    let mut strips: Vec<HStrip> = hbands
        .iter()
        .enumerate()
        .map(|(k, &(x_lo, x_hi))| {
            // The last band's strip also takes the padding columns: a 4-column
            // group that starts before `tw` may write past it, as it always has.
            let w_hi = if k + 1 == nb { stride.max(x_hi) } else { x_hi };
            HStrip {
                x_lo,
                x_hi,
                w: w_hi - x_lo,
                buf: Vec::new(),
            }
        })
        .collect();
    // The horizontal pass reads and writes rows `y0 - 8 ..= y0 + 7` for its
    // own edges `y0 in sp_lo.max(4)..sp_hi`, so a strip holds only those rows:
    // rows outside the span can belong to other threads' work in the pipeline,
    // and copying them back would overwrite it.
    let r_lo = sp_lo.max(4).saturating_sub(8);
    let r_hi = (sp_hi + 8).min(rows);
    let src: &[u16] = &*data;
    crate::par::run_parts_mut(&mut strips, fctx, |strip, fctx| {
        let (x_lo, x_hi, w) = (strip.x_lo, strip.x_hi, strip.w);
        let buf = &mut strip.buf;
        buf.clear();
        for y in r_lo..r_hi {
            buf.extend_from_slice(&src[y * stride + x_lo..][..w]);
        }
        let px: &mut [u16] = buf.as_mut_slice();
        let hctx = EdgeCtx::new(lf, n, plane_idx, 1, chroma, seg_lf_active, fctx);
        let mut y0 = sp_lo.max(4);
        while y0 < sp_hi {
            let mut x0 = x_lo;
            while x0 < x_hi {
                // Strip-local: row `y0` is `y0 - r_lo` rows in, column `x0` is `x0 - x_lo`.
                if x0 + 16 <= x_hi && y0 >= 8 && y0 + 7 <= rows {
                    let ps: [Option<(u8, i32)>; 4] =
                        std::array::from_fn(|k| edge_params(&hctx, x0 + 4 * k, y0));
                    if let Some((len, level)) = ps[0]
                        && ps.iter().all(|p| *p == ps[0])
                            && filter_edge16(
                                px,
                                (y0 - r_lo) * w + (x0 - x_lo),
                                w,
                                false,
                                len,
                                level,
                                lf.sharpness,
                                fctx,
                            )
                        {
                            x0 += 16;
                            continue;
                        }
                    for (k, p) in ps.iter().enumerate() {
                        if let Some((len, level)) = *p {
                            filter_edge(
                                px,
                                (y0 - r_lo) * w + (x0 - x_lo) + 4 * k,
                                1,
                                w as isize,
                                len,
                                level,
                                lf.sharpness,
                                fctx,
                            );
                        }
                    }
                    x0 += 16;
                    continue;
                }
                if let Some((len, level)) = edge_params(&hctx, x0, y0) {
                    if crate::envflags::env_flag!("EC_AV1_DEBLOCK_TRACE")
                        && plane_idx == 0
                        && x0 == 44
                    {
                        eprintln!(
                            "HEDGE x0={x0} y0={y0} len={len} level={level} sharpness={}",
                            lf.sharpness
                        );
                    }
                    filter_edge(
                        px,
                        (y0 - r_lo) * w + (x0 - x_lo),
                        1,
                        w as isize,
                        len,
                        level,
                        lf.sharpness,
                        fctx,
                    );
                }
                x0 += 4;
            }
            y0 += 4;
        }
    });
    for strip in strips.iter().filter(|s| s.w > 0) {
        for (i, row) in strip.buf.chunks_exact(strip.w).enumerate() {
            data[(r_lo + i) * stride + strip.x_lo..][..strip.w].copy_from_slice(row);
        }
    }
}

/// lane-perf6: reused whole-plane `u16` buffers. Every frame takes a
/// pre-CDEF snapshot of all three planes, a post-deblock snapshot when loop
/// restoration is active, and a fresh LR destination per plane -- freshly
/// allocated that is ~25 MB of `mmap`/`munmap` per frame at 4K, so every
/// sample written also takes a first-touch page fault. The buffers are handed
/// back here instead and refilled in place.
///
/// lane-memfix: PROCESS-WIDE, not one stash per thread. These buffers do not
/// stay on one thread: the frame thread allocates the snapshots and a filter
/// worker returns them (the picture's pre-LR planes go back into the pool
/// from whichever worker ran the restoration), so a per-thread pool leaked
/// whole-plane buffers into every worker it touched -- 19 MB a set, times
/// every pool thread the decode ever created. One shared pool with a hard
/// entry cap bounds the retention at [`SCRATCH_CAP`] buffers no matter how
/// many threads cycle through it; anything over the cap is freed.
pub(super) static PLANE_SCRATCH: std::sync::Mutex<Vec<Vec<u16>>> = std::sync::Mutex::new(Vec::new());

/// Most buffers the shared pool keeps. ~3 whole-plane sets per in-flight 4K
/// frame at the shipped `EC_AV1_THREADS` ceiling; the frames actually in
/// flight cycle through far fewer.
pub(super) const SCRATCH_CAP: usize = 32;

/// Takes a pooled buffer for a caller that wants `n` samples, best-fit by
/// CAPACITY so a chroma-sized request does not permanently promote a
/// luma-sized allocation (every pooled buffer would drift to the largest
/// size otherwise). `None` when the pool is empty.
fn scratch_take(n: usize) -> Option<Vec<u16>> {
    let mut p = PLANE_SCRATCH.lock().unwrap();
    if p.is_empty() {
        return None;
    }
    let fit = p
        .iter()
        .enumerate()
        .filter(|(_, v)| v.capacity() >= n)
        .min_by_key(|(_, v)| v.capacity())
        .map(|(i, _)| i);
    let i = match fit {
        Some(i) => i,
        // Nothing big enough: grow the largest rather than a small one.
        None => p
            .iter()
            .enumerate()
            .max_by_key(|(_, v)| v.capacity())
            .map(|(i, _)| i)
            .expect("the pool is not empty"),
    };
    Some(p.swap_remove(i))
}

/// A pooled buffer holding a copy of `src`.
pub(super) fn scratch_from(src: &[u16]) -> Vec<u16> {
    let mut v = scratch_take(src.len()).unwrap_or_default();
    v.clear();
    v.extend_from_slice(src);
    v
}

/// An empty pooled buffer, for a caller that fills it itself.
pub(super) fn scratch_empty() -> Vec<u16> {
    let mut v = scratch_take(0).unwrap_or_default();
    v.clear();
    v
}

/// A pooled buffer of exactly `n` samples whose contents are NOT cleared --
/// for a caller that writes every element it later reads (lane-copy: the
/// post-deblock snapshot writes 4 rows in 64 and reads no other).
pub(super) fn scratch_sized(n: usize) -> Vec<u16> {
    let mut v = scratch_take(n).unwrap_or_default();
    if v.len() > n {
        v.truncate(n);
    } else if v.len() < n {
        v.resize(n, 0);
    }
    v
}

pub(super) fn plane_snapshot(p: &PlaneBuf<'_>, ss_y: u32) -> PlaneBuf<'static> {
    let mut data = scratch_sized(p.data.len());
    let rows = p.data.len().checked_div(p.width).unwrap_or(0);
    let stripe_h = 64usize >> ss_y;
    let offset = 8usize >> ss_y;
    let mut b = stripe_h - offset;
    while b < rows {
        let (lo, hi) = (b - 2, (b + 2).min(rows));
        data[lo * p.width..hi * p.width].copy_from_slice(&p.data[lo * p.width..hi * p.width]);
        b += stripe_h;
    }
    PlaneBuf {
        data: std::borrow::Cow::Owned(data),
        ..*p
    }
}

/// The pre-filter reconstruction of one coded frame, plus the per-block state
/// the loop filters read.
///
/// The encoder's filter search ([`crate::filter_search::pick_filters`])
/// decodes the same coded tile under ~25 candidate deblock/CDEF parameter
/// sets to score each one, and the entropy decode and reconstruction it
/// repeats are identical every time -- both filters run after it, on the
/// finished planes. That re-parse was 24% of the encoder's profile. A caller
/// arms [`arm_filter_replay`] around ONE decode and then re-runs the filters
/// alone through [`replay_filters`]; the result is the same picture the full
/// decode would have returned, because it is the same two calls on a copy of
/// the same planes.
pub(crate) struct FilterReplay {
    pub(super) y: PlaneBuf<'static>,
    pub(super) u: PlaneBuf<'static>,
    pub(super) v: PlaneBuf<'static>,
    pub(super) neighbours: Neighbours,
    pub(super) frame_width: usize,
    pub(super) frame_height: usize,
    pub(super) width: usize,
    pub(super) height: usize,
    /// The reconstructed extent (`mi_cols`/`mi_rows` * 4), for the wide-margin
    /// stash the cropped return path leaves behind.
    pub(super) true_width: usize,
    pub(super) true_height: usize,
    /// lane-av1fpar: plane buffers this replay is done with. Every candidate
    /// works on its own copy of three ~6 MB planes, and a fresh `Vec` per
    /// copy is a fresh mmap the kernel faults in page by page -- 12.6% of a
    /// 1080p frame's wall in allocation, not in filtering. Recycled here
    /// instead; the pool holds at most the six planes one candidate has live.
    pub(super) scratch: Vec<Vec<u16>>,
    /// lane-av1cap2: the final replay's post-CDEF luma plane and the TRUE
    /// post-deblock rows loop restoration substitutes at its stripe
    /// borders ([`plane_snapshot`], exactly `restoration::lr_src_row`'s
    /// rows) -- what [`replay_restoration`] filters, instead of decoding
    /// the re-coded tile a second time.
    pub(super) final_luma: Option<(PlaneBuf<'static>, PlaneBuf<'static>)>,
    /// lane-av1speed3: the previous candidate's DESTINATION planes, still
    /// holding its post-CDEF samples, and one byte per 8x8 CDEF unit its
    /// filter wrote (see [`cdef_band`]). The CDEF half of the search moves
    /// no deblocking level, so every candidate of it used to copy the three
    /// post-deblock planes (~19 MB at 4K) into fresh destinations. Here the
    /// destinations are kept and only the units the last candidate touched
    /// are restored -- the same bytes, since a unit nobody wrote still holds
    /// the deblocked sample it was copied from.
    pub(super) work: Option<(PlaneBuf<'static>, PlaneBuf<'static>, PlaneBuf<'static>)>,
    pub(super) wrote: Vec<u8>,
    /// The strengths [`FilterReplay::work`] was filtered under: a plane whose
    /// new candidate can only write MORE units than the old one needs no
    /// restore at all, since every unit CDEF writes it writes from the
    /// post-deblock source alone.
    pub(super) work_cdef: Option<CdefParams>,
    /// The last deblocked planes and the levels that produced them.
    /// Deblocking depends on nothing else the search moves, and the search
    /// fixes the levels before it walks the CDEF strengths, so every
    /// candidate after the first of a level pair replays CDEF alone.
    pub(super) deblocked: Option<(
        LoopFilterParams,
        PlaneBuf<'static>,
        PlaneBuf<'static>,
        PlaneBuf<'static>,
    )>,
}

thread_local! {
    /// Set by [`arm_filter_replay`] for the span of one decode.
    static REPLAY_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// What that decode captured, until [`clear_filter_replay`].
    pub(super) static REPLAY: std::cell::RefCell<Option<FilterReplay>> =
        const { std::cell::RefCell::new(None) };
}

/// Capture this thread's next decode's pre-filter frame (see [`FilterReplay`]).
pub(crate) fn arm_filter_replay() {
    REPLAY_ARMED.set(true);
}

/// Test-only: force every filter candidate through a full decode, so a gate
/// can hold the replay against the decode it stands in for
/// (`encoder::tests::filter_replay_codes_the_same_stream_as_a_full_decode`).
#[cfg(test)]
pub(super) static REPLAY_DISABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn set_filter_replay_disabled(off: bool) {
    REPLAY_DISABLED.store(off, std::sync::atomic::Ordering::Relaxed);
}

// Test-only: run the capture decode `replay_final` replaced as well, and
// hold its picture and its `FilterStages` against the replay's
// (`encoder::tests::filter_replay_final_matches_the_capture_decode`).
// Thread-local, not a global: the filter stage runs on the frame thread the
// test itself drives, and a process-wide flag would put every test encoding
// in parallel with this one through the extra decode too.
#[cfg(test)]
thread_local! {
    static VERIFY_FINAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn set_verify_final_replay(on: bool) {
    VERIFY_FINAL.set(on);
}

#[cfg(test)]
pub(crate) fn verify_final_replay() -> bool {
    VERIFY_FINAL.get()
}

/// Drop the armed flag and anything captured: the tile it belongs to is done.
pub(crate) fn clear_filter_replay() {
    REPLAY_ARMED.set(false);
    REPLAY.with_borrow_mut(|r| *r = None);
}

/// The capture itself, called from the decoder's own pre-filter point. Owns
/// copies of the planes, since the caller filters them in place.
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn capture_filter_replay(
    y: &PlaneBuf<'_>,
    u: &PlaneBuf<'_>,
    v: &PlaneBuf<'_>,
    neighbours: &Neighbours,
    frame_width: usize,
    frame_height: usize,
    width: usize,
    height: usize,
    true_width: usize,
    true_height: usize,
) {
    if !REPLAY_ARMED.replace(false) {
        return;
    }
    let own = |p: &PlaneBuf<'_>| PlaneBuf {
        data: std::borrow::Cow::Owned(p.data.to_vec()),
        ..*p
    };
    REPLAY.with_borrow_mut(|slot| {
        *slot = Some(FilterReplay {
            y: own(y),
            u: own(u),
            v: own(v),
            neighbours: neighbours.clone(),
            frame_width,
            frame_height,
            width,
            height,
            true_width,
            true_height,
            scratch: Vec::new(),
            work: None,
            wrote: Vec::new(),
            work_cdef: None,
            final_luma: None,
            deblocked: None,
        });
    });
}

/// Restores, from the post-deblock cache, every sample the previous CDEF
/// candidate may have written: for each unit row, the column span between
/// its first and last written unit (a superset of the written units, which
/// is what makes the copy contiguous -- and never more than the whole-plane
/// copy it replaces). `unit` is this plane's CDEF unit side, 8 for luma and
/// 4 for 4:2:0 chroma.
pub(super) fn restore_written(
    dst: &mut PlaneBuf<'static>,
    src: &PlaneBuf<'_>,
    wrote: &[u8],
    mask: u8,
    unit_cols: usize,
    unit: usize,
) {
    let w = dst.width;
    if w == 0 || unit_cols == 0 || wrote.is_empty() {
        return;
    }
    let rows = dst.data.len() / w;
    let s: &[u16] = &src.data;
    let d = dst.data.to_mut();
    let band = |d: &mut [u16], wr: &[u8], ur0: usize| {
        for (i, row) in wr.chunks(unit_cols).enumerate() {
            let Some(c0) = row.iter().position(|&b| b & mask != 0) else {
                continue;
            };
            let c1 = row
                .iter()
                .rposition(|&b| b & mask != 0)
                .expect("just found one");
            let (x0, x1) = ((c0 * unit).min(w), ((c1 + 1) * unit).min(w));
            let ur = ur0 + i;
            let (y0, y1) = ((ur * unit).min(rows), ((ur + 1) * unit).min(rows));
            for r in y0..y1 {
                let off = (r - ur0 * unit) * w;
                d[off + x0..off + x1].copy_from_slice(&s[r * w + x0..r * w + x1]);
            }
        }
    };
    let urows = wrote.len() / unit_cols;
    let threads = crate::par::filter_threads().min(urows.max(1));
    if threads <= 1 {
        band(d, wrote, 0);
        return;
    }
    let per = urows.div_ceil(threads);
    crate::par::scope("ec-av1-filter", |batch| {
        for (i, (dc, wr)) in d
            .chunks_mut(per * unit * w)
            .zip(wrote.chunks(per * unit_cols))
            .enumerate()
        {
            let band = &band;
            batch.submit(move || band(dc, wr, i * per));
        }
    });
}

/// One candidate's working copy of a captured plane, off [`FilterReplay`]'s
/// own buffer pool: a cleared `Vec` that already has the capacity keeps the
/// allocator and the page fault handler out of the filter search.
pub(super) fn pooled(p: &PlaneBuf<'_>, scratch: &mut Vec<Vec<u16>>) -> PlaneBuf<'static> {
    let mut buf = scratch.pop().unwrap_or_default();
    let src: &[u16] = &p.data;
    let threads = crate::par::filter_threads();
    if buf.len() != src.len() || threads <= 1 {
        buf.clear();
        buf.extend_from_slice(src);
    } else {
        // A recycled buffer is already the right length, so the copy is a
        // plain `copy_from_slice` -- and one worth banding: ~6 MB per
        // candidate is memory bandwidth, not arithmetic, and the halves are
        // disjoint `&mut` slices (no `par::Shared` needed).
        let per = src.len().div_ceil(threads);
        crate::par::scope("ec-av1-filter", |batch| {
            for (dst, src) in buf.chunks_mut(per).zip(src.chunks(per)) {
                batch.submit(move || dst.copy_from_slice(src));
            }
        });
    }
    PlaneBuf {
        data: std::borrow::Cow::Owned(buf),
        ..*p
    }
}

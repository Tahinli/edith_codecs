//! VP8 in-loop deblocking filter — RFC 6386 Section 15.
//!
//! Implements the *normal* filter (§15.3), the *simple* filter (§15.2), the
//! per-frame macroblock-edge driver in the reference decoder's exact visit
//! order, and the per-sharpness limit-table construction of §15.4.
//!
//! The arithmetic is transcribed line-for-line from the normative reference
//! sources and is bit-identical between them (verified term-by-term):
//!
//! * `dixie_loopfilter.c` shipped with RFC 6386 (§20.6) — kernels
//!   (`filter_common`, `filter_mb_edge`, thresholds) and the
//!   `filter_row_normal` / `filter_row_simple` drivers,
//! * libvpx `vp8/common/loopfilter_filters.c` — the same kernels as
//!   branch-free C, and `vp8/common/vp8_loopfilter.c`
//!   (`vp8_loop_filter_update_sharpness`, `lf_init_lut`) — the limit and
//!   high-edge-variance tables.
//!
//! C signed right-shifts are arithmetic (floor); Rust `i32 >>` has the same
//! semantics, so every `>>` below reproduces the C rounding exactly. No
//! allocation, no `unsafe`.
//!
//! Frame-type note: §15.4 (and libvpx `lf_init_lut`) makes `hev_threshold`
//! depend on whether the current frame is a key frame. [`filter_frame`]
//! keeps the crate-contract signature and applies the *inter-frame* table;
//! call [`filter_frame_ex`] with the decoded `is_keyframe` flag for fully
//! type-exact decoding (the two differ only for levels ≥ 15).

/// Per-macroblock filter inputs the decoder derives from the frame header,
/// segmentation map and per-MB prediction info.
///
/// `level` is the *final* per-MB loop filter level (0..=63) after the
/// segment and ref/mode delta adjustments of RFC 6386 §15.4 (implemented
/// decoder-side in `decode.rs`, mirroring dixie `calculate_filter_parameters`).
/// A level of 0 disables all filtering of that macroblock (dixie:
/// `if (edge_limit) { ... }`).
///
/// `inner_edges` controls whether the subblock (interior) edges of the MB
/// are filtered: true iff the MB has any non-zero DCT coefficient or its
/// mode is `B_PRED`/`SPLITMV` (RFC §15.1; dixie's `eob_mask` condition,
/// `filter_row_normal` ~L9308 and `filter_row_simple` ~L9433).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MbFilterInfo {
    /// Effective per-MB loop filter level after segment and ref/mode delta
    /// adjustments, clamped to 0..=63.
    pub level: u8,
    /// Filter interior (subblock) edges of this MB (any non-zero DCT
    /// coefficient, or `B_PRED`/`SPLITMV` mode).
    pub inner_edges: bool,
}

/// Per-sharpness limit tables for levels 0..=63, in the order
/// `lim` / `blim` / `mblim` of libvpx `loop_filter_info_n`.
///
/// These are the combined edge limits of RFC §15.4:
/// `mblim[l] = (l + 2) * 2 + interior_limit(l)` (macroblock edges),
/// `blim[l] = l * 2 + interior_limit(l)` (subblock edges), and
/// `lim[l] = interior_limit(l)`. Both filter variants consume these exact
/// combined values (the simple filter directly, as dixie
/// `filter_v_edge_simple` does; the normal filter splits them back into
/// center + interior tests).
#[derive(Clone, Copy, Debug)]
pub struct LoopFilterTables {
    /// Interior-difference limit per level (dixie `interior_limit`).
    pub lim: [u8; 64],
    /// Combined subblock-edge center limit per level (dixie `b_limit`).
    pub blim: [u8; 64],
    /// Combined macroblock-edge center limit per level (dixie `mb_limit`).
    pub mblim: [u8; 64],
}

impl LoopFilterTables {
    /// Build the three limit tables for one frame's sharpness level.
    ///
    /// Port of libvpx `vp8_loop_filter_update_sharpness`
    /// (vp8/common/vp8_loopfilter.c), which matches the RFC §15.4
    /// pseudocode:
    ///
    /// ```text
    /// inside = level >> (sharpness > 0) >> (sharpness > 4)
    /// if sharpness > 0 and inside > 9 - sharpness: inside = 9 - sharpness
    /// inside = max(inside, 1)
    /// ```
    pub fn update_sharpness(sharpness: u8) -> Self {
        let s = i32::from(sharpness);
        let mut t = LoopFilterTables {
            lim: [0; 64],
            blim: [0; 64],
            mblim: [0; 64],
        };
        for (lvl, slot) in t.lim.iter_mut().enumerate() {
            let filt_lvl = lvl as i32;
            // libvpx L59-60: two conditional single-bit shifts.
            let mut inside = filt_lvl;
            if s > 0 {
                inside >>= 1;
            }
            if s > 4 {
                inside >>= 1;
            }
            // libvpx L62-66: cap only applies when sharpness > 0.
            if s > 0 && inside > 9 - s {
                inside = 9 - s;
            }
            if inside < 1 {
                inside = 1;
            }
            *slot = inside as u8;
            t.blim[lvl] = (2 * filt_lvl + inside) as u8;
            t.mblim[lvl] = (2 * (filt_lvl + 2) + inside) as u8;
        }
        t
    }
}

/// Interior-difference limit for one level/sharpness pair (RFC §15.4).
///
/// Same derivation as [`LoopFilterTables::update_sharpness`] for a single
/// level; provided for callers that need the value without the tables.
pub fn interior_limit(level: u8, sharpness: u8) -> i32 {
    let s = i32::from(sharpness);
    let mut il = i32::from(level);
    if s > 0 {
        // dixie: interior_limit >>= sharpness > 4 ? 2 : 1 — the two
        // conditional single-bit shifts of update_sharpness folded.
        il >>= if s > 4 { 2 } else { 1 };
        if il > 9 - s {
            il = 9 - s;
        }
    }
    if il < 1 {
        il = 1;
    }
    il
}

/// High-edge-variance threshold for one level (RFC §15.4, last block; also
/// libvpx `lf_init_lut`'s `hev_thr_lut[KEY_FRAME|INTER_FRAME]`).
///
/// Key frames: 2 at level ≥ 40, 1 at level ≥ 15, else 0. Inter frames: 3 at
/// level ≥ 40, 2 at level ≥ 20, 1 at level ≥ 15, else 0.
pub fn hev_threshold(level: u8, is_keyframe: bool) -> i32 {
    let l = i32::from(level);
    if is_keyframe {
        if l >= 40 {
            2
        } else if l >= 15 {
            1
        } else {
            0
        }
    } else if l >= 40 {
        3
    } else if l >= 20 {
        2
    } else if l >= 15 {
        1
    } else {
        0
    }
}

/// Filter one frame's worth of macroblock edges, in the reference decoder's
/// exact order (dixie `filter_row_normal` / `filter_row_simple`, applied
/// row-major over MBs; each MB visits, when enabled: left MB V-edge, then
/// subblock V-edges at +4/+8/+12, then top MB H-edge, then subblock
/// H-edges at +4/+8/+12 rows).
///
/// `y`/`u`/`v` are unbordered plane buffers starting at pixel (0,0),
/// `stride`/`uv_stride` samples apart, sized `mb_cols*16 × mb_rows*16`
/// (and halved for chroma). Frame-boundary edges (column 0, row 0) are not
/// filtered. `mb_info` is indexed `row * mb_cols + col`; a level of 0
/// disables all filtering of that MB. The simple variant filters only the
/// Y plane (dixie `filter_row_simple`).
///
/// This applies the inter-frame `hev_threshold` table (see the module docs);
/// use [`filter_frame_ex`] for frame-type-exact decoding.
#[allow(clippy::too_many_arguments)]
pub fn filter_frame(
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    stride: usize,
    uv_stride: usize,
    mb_cols: usize,
    mb_rows: usize,
    sharpness: u8,
    simple_filter_type: bool,
    mb_info: &[MbFilterInfo],
) {
    filter_frame_ex(
        y,
        u,
        v,
        stride,
        uv_stride,
        mb_cols,
        mb_rows,
        sharpness,
        simple_filter_type,
        mb_info,
        false,
    );
}

/// [`filter_frame`] with an explicit frame type: `is_keyframe` selects the
/// §15.4 key-frame `hev_threshold` table instead of the inter-frame table
/// (dixie `calculate_filter_parameters`:
/// `if (filter_level >= 20 && !is_keyframe) hev_threshold++`).
#[allow(clippy::too_many_arguments)]
pub fn filter_frame_ex(
    y: &mut [u8],
    u: &mut [u8],
    v: &mut [u8],
    stride: usize,
    uv_stride: usize,
    mb_cols: usize,
    mb_rows: usize,
    sharpness: u8,
    simple_filter_type: bool,
    mb_info: &[MbFilterInfo],
    is_keyframe: bool,
) {
    debug_assert_eq!(mb_info.len(), mb_cols * mb_rows);
    debug_assert!(y.len() >= stride * (mb_rows * 16 - 1) + mb_cols * 16);
    debug_assert!(u.len() >= uv_stride * (mb_rows * 8 - 1) + mb_cols * 8);
    debug_assert!(v.len() >= uv_stride * (mb_rows * 8 - 1) + mb_cols * 8);
    debug_assert!(stride >= mb_cols * 16);
    debug_assert!(uv_stride >= mb_cols * 8);

    let tables = LoopFilterTables::update_sharpness(sharpness);

    for row in 0..mb_rows {
        for col in 0..mb_cols {
            let info = &mb_info[row * mb_cols + col];
            let level = info.level as usize;
            // dixie filter_row_normal L9280 / filter_row_simple L9425:
            // `if (edge_limit)` — level 0 filters nothing for this MB.
            if level == 0 {
                continue;
            }
            let hev = hev_threshold(info.level, is_keyframe);
            let mblim = i32::from(tables.mblim[level]);
            let blim = i32::from(tables.blim[level]);
            let lim = i32::from(tables.lim[level]);
            let ybase = row * 16 * stride + col * 16;
            let uvbase = row * 8 * uv_stride + col * 8;

            if simple_filter_type {
                // dixie filter_row_simple L9436-9460. Y plane only.
                if col > 0 {
                    filter_v_edge_simple(y, stride, ybase, mblim);
                }
                if info.inner_edges {
                    filter_v_edge_simple(y, stride, ybase + 4, blim);
                    filter_v_edge_simple(y, stride, ybase + 8, blim);
                    filter_v_edge_simple(y, stride, ybase + 12, blim);
                }
                if row > 0 {
                    filter_h_edge_simple(y, stride, ybase, mblim);
                }
                if info.inner_edges {
                    filter_h_edge_simple(y, stride, ybase + 4 * stride, blim);
                    filter_h_edge_simple(y, stride, ybase + 8 * stride, blim);
                    filter_h_edge_simple(y, stride, ybase + 12 * stride, blim);
                }
                continue;
            }

            // dixie filter_row_normal L9282-9378.
            if col > 0 {
                filter_mb_v_edge(y, stride, ybase, mblim, lim, hev, 2);
                filter_mb_v_edge(u, uv_stride, uvbase, mblim, lim, hev, 1);
                filter_mb_v_edge(v, uv_stride, uvbase, mblim, lim, hev, 1);
            }
            if info.inner_edges {
                filter_subblock_v_edge(y, stride, ybase + 4, blim, lim, hev, 2);
                filter_subblock_v_edge(y, stride, ybase + 8, blim, lim, hev, 2);
                filter_subblock_v_edge(y, stride, ybase + 12, blim, lim, hev, 2);
                filter_subblock_v_edge(u, uv_stride, uvbase + 4, blim, lim, hev, 1);
                filter_subblock_v_edge(v, uv_stride, uvbase + 4, blim, lim, hev, 1);
            }
            if row > 0 {
                filter_mb_h_edge(y, stride, ybase, mblim, lim, hev, 2);
                filter_mb_h_edge(u, uv_stride, uvbase, mblim, lim, hev, 1);
                filter_mb_h_edge(v, uv_stride, uvbase, mblim, lim, hev, 1);
            }
            if info.inner_edges {
                filter_subblock_h_edge(y, stride, ybase + 4 * stride, blim, lim, hev, 2);
                filter_subblock_h_edge(y, stride, ybase + 8 * stride, blim, lim, hev, 2);
                filter_subblock_h_edge(y, stride, ybase + 12 * stride, blim, lim, hev, 2);
                filter_subblock_h_edge(u, uv_stride, uvbase + 4 * uv_stride, blim, lim, hev, 1);
                filter_subblock_h_edge(v, uv_stride, uvbase + 4 * uv_stride, blim, lim, hev, 1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Kernels — transcriptions of dixie_loopfilter.c (RFC 6386 §20.6).
// Pixels are handled as raw unsigned samples widened to `i32`; every clamp
// is the exact dixie saturate_int8 / saturate_uint8.
// ---------------------------------------------------------------------------

/// dixie `saturate_int8`.
#[inline]
fn sat_i8(x: i32) -> i32 {
    x.clamp(-128, 127)
}

/// dixie `saturate_uint8` (result kept as `i32` in 0..=255).
#[inline]
fn sat_u8(x: i32) -> i32 {
    x.clamp(0, 255)
}

/// dixie `simple_threshold` (RFC §15.2/§15.3): the center test
/// `|p0-q0|*2 + |p1-q1|/2 <= limit` with `limit` already the *combined*
/// edge limit (`2*E + I`, or the tables' `blim`/`mblim`).
#[inline]
fn simple_threshold(p1: i32, p0: i32, q0: i32, q1: i32, limit: i32) -> bool {
    (p0 - q0).abs() * 2 + ((p1 - q1).abs() >> 1) <= limit
}

/// dixie `normal_threshold` (RFC §15.3 `filter_yes`): the combined center
/// test plus all six interior-difference tests against `interior`.
/// `px` is `[p3, p2, p1, p0, q0, q1, q2, q3]` across the edge.
#[inline]
fn normal_threshold(px: &[i32; 8], center_limit: i32, interior: i32) -> bool {
    let [_, p2, p1, p0, q0, q1, q2, _] = *px;
    simple_threshold(p1, p0, q0, q1, center_limit)
        && (px[0] - p2).abs() <= interior
        && (p2 - p1).abs() <= interior
        && (p1 - p0).abs() <= interior
        && (px[7] - q2).abs() <= interior
        && (q2 - q1).abs() <= interior
        && (q1 - q0).abs() <= interior
}

/// dixie `high_edge_variance` (RFC §15.3 `hev`).
#[inline]
fn high_edge_variance(p1: i32, p0: i32, q0: i32, q1: i32, hev_threshold: i32) -> bool {
    (p1 - p0).abs() > hev_threshold || (q1 - q0).abs() > hev_threshold
}

/// dixie `filter_common` (RFC §15.3 `common_adjust`).
///
/// `use_outer_taps` is true for the simple filter and for high-variance
/// edges. Returns `[p1, p0, q0, q1]`; the returned `p1`/`q1` equal the
/// inputs unless `!use_outer_taps` (subblock filter adjusts the inner
/// taps by half the edge adjustment).
fn filter_common(p1: i32, p0: i32, q0: i32, q1: i32, use_outer_taps: bool) -> [i32; 4] {
    let mut a = 3 * (q0 - p0);
    if use_outer_taps {
        a += sat_i8(p1 - q1);
    }
    a = sat_i8(a);
    // dixie clamps only the top here; a ≥ -128 makes a bottom clamp dead
    // code, and the arithmetic `>> 3` reproduces the C rounding exactly.
    let f1 = (if a + 4 > 127 { 127 } else { a + 4 }) >> 3;
    let f2 = (if a + 3 > 127 { 127 } else { a + 3 }) >> 3;
    let p0 = sat_u8(p0 + f2);
    let q0 = sat_u8(q0 - f1);
    let (p1, q1) = if !use_outer_taps {
        let a = (f1 + 1) >> 1;
        (sat_u8(p1 + a), sat_u8(q1 - a))
    } else {
        (p1, q1)
    };
    [p1, p0, q0, q1]
}

/// dixie `filter_mb_edge` (RFC §15.3 `MBfilter`, non-hev branch): `w` is
/// "twice the edge difference" and the 27/18/9 coefficients approximate
/// 3/7, 2/7, 1/7 of it. Returns `[p2, p1, p0, q0, q1, q2]`.
fn filter_mb_edge(p2: i32, p1: i32, p0: i32, q0: i32, q1: i32, q2: i32) -> [i32; 6] {
    let w = sat_i8(sat_i8(p1 - q1) + 3 * (q0 - p0));
    let a = (27 * w + 63) >> 7;
    let (p0, q0) = (sat_u8(p0 + a), sat_u8(q0 - a));
    let a = (18 * w + 63) >> 7;
    let (p1, q1) = (sat_u8(p1 + a), sat_u8(q1 - a));
    let a = (9 * w + 63) >> 7;
    let (p2, q2) = (sat_u8(p2 + a), sat_u8(q2 - a));
    [p2, p1, p0, q0, q1, q2]
}

// ---------------------------------------------------------------------------
// Edge walkers. `anchor` is the flat index of the edge's q0 sample in the
// first row (V edges) / column (H edges); `size` is the number of 8-pixel
// segments (2 = 16-pixel Y MB edge, 1 = 8-pixel chroma / subblock edge).
// Frame-boundary guarantees (anchor ≥ 4 samples from the plane edge) are
// established by `filter_frame_ex`'s `col > 0` / `row > 0` guards.
// ---------------------------------------------------------------------------

/// dixie `filter_mb_v_edge`: vertical MB edge, normal filter.
fn filter_mb_v_edge(
    plane: &mut [u8],
    stride: usize,
    anchor: usize,
    center_limit: i32,
    interior: i32,
    hev: i32,
    size: usize,
) {
    // Taps are contiguous around each anchor: one slice read per pixel;
    // most pixels fail the threshold, so stores stay conditional.
    for row in 0..8 * size {
        let a = anchor + row * stride;
        debug_assert!(a >= 4 && a + 3 < plane.len());
        let mut px = [0u8; 8];
        px.copy_from_slice(&plane[a - 4..a + 4]);
        let c = [
            i32::from(px[0]),
            i32::from(px[1]),
            i32::from(px[2]),
            i32::from(px[3]),
            i32::from(px[4]),
            i32::from(px[5]),
            i32::from(px[6]),
            i32::from(px[7]),
        ];
        if !normal_threshold(&c, center_limit, interior) {
            continue;
        }
        let [_, p2, p1, p0, q0, q1, q2, _] = c;
        if high_edge_variance(p1, p0, q0, q1, hev) {
            // dixie: filter_common with outer taps (p1/q1 unchanged).
            let f = filter_common(p1, p0, q0, q1, true);
            px[2] = f[0] as u8;
            px[3] = f[1] as u8;
            px[4] = f[2] as u8;
            px[5] = f[3] as u8;
        } else {
            let m = filter_mb_edge(p2, p1, p0, q0, q1, q2);
            px[1] = m[0] as u8;
            px[2] = m[1] as u8;
            px[3] = m[2] as u8;
            px[4] = m[3] as u8;
            px[5] = m[4] as u8;
            px[6] = m[5] as u8;
        }
        plane[a - 3..a + 3].copy_from_slice(&px[1..7]);
    }
}

/// dixie `filter_subblock_v_edge`: vertical subblock edge, normal filter.
fn filter_subblock_v_edge(
    plane: &mut [u8],
    stride: usize,
    anchor: usize,
    center_limit: i32,
    interior: i32,
    hev: i32,
    size: usize,
) {
    for row in 0..8 * size {
        let a = anchor + row * stride;
        debug_assert!(a >= 4 && a + 3 < plane.len());
        let px: [u8; 8] = plane[a - 4..a + 4].try_into().unwrap();
        let c = [
            i32::from(px[0]),
            i32::from(px[1]),
            i32::from(px[2]),
            i32::from(px[3]),
            i32::from(px[4]),
            i32::from(px[5]),
            i32::from(px[6]),
            i32::from(px[7]),
        ];
        if normal_threshold(&c, center_limit, interior) {
            let outer = high_edge_variance(c[2], c[3], c[4], c[5], hev);
            let f = filter_common(c[2], c[3], c[4], c[5], outer);
            plane[a - 2] = f[0] as u8;
            plane[a - 1] = f[1] as u8;
            plane[a] = f[2] as u8;
            plane[a + 1] = f[3] as u8;
        }
    }
}

/// dixie `filter_mb_h_edge`: horizontal MB edge, normal filter.
fn filter_mb_h_edge(
    plane: &mut [u8],
    stride: usize,
    anchor: usize,
    center_limit: i32,
    interior: i32,
    hev: i32,
    size: usize,
) {
    // Columns are contiguous (anchor + i), taps sit `stride` apart: the
    // tap rows are pulled in with one bounds-checked slice each (most
    // columns fail the threshold and need no store, so writes stay
    // per-column).
    let n = 8 * size;
    let mut px = [[0u8; 64]; 8];
    for (k, r) in px.iter_mut().enumerate() {
        let a = anchor - (4 - k) * stride;
        r[..n].copy_from_slice(&plane[a..a + n]);
    }
    for i in 0..n {
        let c = [
            i32::from(px[0][i]),
            i32::from(px[1][i]),
            i32::from(px[2][i]),
            i32::from(px[3][i]),
            i32::from(px[4][i]),
            i32::from(px[5][i]),
            i32::from(px[6][i]),
            i32::from(px[7][i]),
        ];
        if !normal_threshold(&c, center_limit, interior) {
            continue;
        }
        let [_, p2, p1, p0, q0, q1, q2, _] = c;
        if high_edge_variance(p1, p0, q0, q1, hev) {
            let f = filter_common(p1, p0, q0, q1, true);
            plane[anchor - 2 * stride + i] = f[0] as u8;
            plane[anchor - stride + i] = f[1] as u8;
            plane[anchor + i] = f[2] as u8;
            plane[anchor + stride + i] = f[3] as u8;
        } else {
            let m = filter_mb_edge(p2, p1, p0, q0, q1, q2);
            plane[anchor - 3 * stride + i] = m[0] as u8;
            plane[anchor - 2 * stride + i] = m[1] as u8;
            plane[anchor - stride + i] = m[2] as u8;
            plane[anchor + i] = m[3] as u8;
            plane[anchor + stride + i] = m[4] as u8;
            plane[anchor + 2 * stride + i] = m[5] as u8;
        }
    }
}

/// dixie `filter_subblock_h_edge`: horizontal subblock edge, normal filter.
fn filter_subblock_h_edge(
    plane: &mut [u8],
    stride: usize,
    anchor: usize,
    center_limit: i32,
    interior: i32,
    hev: i32,
    size: usize,
) {
    let n = 8 * size;
    let mut px = [[0u8; 64]; 8];
    for (k, r) in px.iter_mut().enumerate() {
        let a = anchor - (4 - k) * stride;
        r[..n].copy_from_slice(&plane[a..a + n]);
    }
    for i in 0..n {
        let c = [
            i32::from(px[0][i]),
            i32::from(px[1][i]),
            i32::from(px[2][i]),
            i32::from(px[3][i]),
            i32::from(px[4][i]),
            i32::from(px[5][i]),
            i32::from(px[6][i]),
            i32::from(px[7][i]),
        ];
        if normal_threshold(&c, center_limit, interior) {
            let outer = high_edge_variance(c[2], c[3], c[4], c[5], hev);
            let f = filter_common(c[2], c[3], c[4], c[5], outer);
            plane[anchor - 2 * stride + i] = f[0] as u8;
            plane[anchor - stride + i] = f[1] as u8;
            plane[anchor + i] = f[2] as u8;
            plane[anchor + stride + i] = f[3] as u8;
        }
    }
}

/// dixie `filter_v_edge_simple`: vertical edge, simple filter (2-tap;
/// consumes the combined limit directly, touches only p0/q0).
fn filter_v_edge_simple(plane: &mut [u8], stride: usize, anchor: usize, limit: i32) {
    for row in 0..16 {
        let a = anchor + row * stride;
        debug_assert!(a >= 2 && a + 1 < plane.len());
        let px: [u8; 4] = plane[a - 2..a + 2].try_into().unwrap();
        let p1 = i32::from(px[0]);
        let p0 = i32::from(px[1]);
        let q0 = i32::from(px[2]);
        let q1 = i32::from(px[3]);
        if simple_threshold(p1, p0, q0, q1, limit) {
            let f = filter_common(p1, p0, q0, q1, true);
            plane[a - 1] = f[1] as u8;
            plane[a] = f[2] as u8;
        }
    }
}

/// dixie `filter_h_edge_simple`: horizontal edge, simple filter.
fn filter_h_edge_simple(plane: &mut [u8], stride: usize, anchor: usize, limit: i32) {
    // Contiguous columns, strided taps: chunked loads, conditional stores.
    let mut px = [[0u8; 16]; 4];
    for (k, r) in px.iter_mut().enumerate() {
        let a = anchor - (2 - k) * stride;
        r.copy_from_slice(&plane[a..a + 16]);
    }
    for i in 0..16 {
        let p1 = i32::from(px[0][i]);
        let p0 = i32::from(px[1][i]);
        let q0 = i32::from(px[2][i]);
        let q1 = i32::from(px[3][i]);
        if simple_threshold(p1, p0, q0, q1, limit) {
            let f = filter_common(p1, p0, q0, q1, true);
            plane[anchor - stride + i] = f[1] as u8;
            plane[anchor + i] = f[2] as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Python reference model — the SAME integer math used to derive every
    // expected vector below (transcribed from dixie_loopfilter.c, RFC 6386
    // §20.6; control params from §15.4 / libvpx vp8_loopfilter.c). Run to
    // regenerate the constants in these tests.
    //
    //   def sat_i8(x): return -128 if x < -128 else (127 if x > 127 else x)
    //   def sat_u8(x): return 0 if x < 0 else (255 if x > 255 else x)
    //
    //   def simple_threshold(p1, p0, q0, q1, limit):
    //       return (abs(p0 - q0) * 2 + (abs(p1 - q1) >> 1)) <= limit
    //
    //   def normal_threshold(p3, p2, p1, p0, q0, q1, q2, q3, E, I):
    //       return simple_threshold(p1, p0, q0, q1, 2 * E + I) and \
    //           abs(p3 - p2) <= I and abs(p2 - p1) <= I and abs(p1 - p0) <= I and \
    //           abs(q3 - q2) <= I and abs(q2 - q1) <= I and abs(q1 - q0) <= I
    //
    //   def high_edge_variance(p1, p0, q0, q1, t):
    //       return abs(p1 - p0) > t or abs(q1 - q0) > t
    //
    //   def filter_common(p1, p0, q0, q1, outer):
    //       a = 3 * (q0 - p0)
    //       if outer: a += sat_i8(p1 - q1)
    //       a = sat_i8(a)
    //       f1 = (127 if a + 4 > 127 else a + 4) >> 3
    //       f2 = (127 if a + 3 > 127 else a + 3) >> 3
    //       p0 = sat_u8(p0 + f2); q0 = sat_u8(q0 - f1)
    //       if not outer:
    //           a = (f1 + 1) >> 1
    //           p1 = sat_u8(p1 + a); q1 = sat_u8(q1 - a)
    //       return p1, p0, q0, q1
    //
    //   def filter_mb_edge(p2, p1, p0, q0, q1, q2):
    //       w = sat_i8(sat_i8(p1 - q1) + 3 * (q0 - p0))
    //       a = (27 * w + 63) >> 7; p0 = sat_u8(p0 + a); q0 = sat_u8(q0 - a)
    //       a = (18 * w + 63) >> 7; p1 = sat_u8(p1 + a); q1 = sat_u8(q1 - a)
    //       a = (9 * w + 63) >> 7;  p2 = sat_u8(p2 + a); q2 = sat_u8(q2 - a)
    //       return p2, p1, p0, q0, q1, q2
    //
    //   def interior_limit(level, sharpness):
    //       il = level
    //       if sharpness:
    //           il >>= (2 if sharpness > 4 else 1)
    //           if il > 9 - sharpness: il = 9 - sharpness
    //       return il if il >= 1 else 1
    //
    //   def hev_threshold(level, key):
    //       if key:  return 2 if level >= 40 else (1 if level >= 15 else 0)
    //       return 3 if level >= 40 else (2 if level >= 20 else (1 if level >= 15 else 0))
    //
    //   # update_sharpness: inside = lvl >> (s>0) >> (s>4), capped at 9-s
    //   # (only when s>0), min 1; lim=inside, blim=2*lvl+inside,
    //   # mblim=2*(lvl+2)+inside.
    //   #
    //   # Edge walkers mirror the Rust walkers: per row (V) / column (H),
    //   # threshold -> hev ? filter_common(outer=True) : filter_mb_edge for
    //   # MB edges, threshold -> filter_common(hev) for subblock edges,
    //   # threshold -> filter_common(True) touching p0/q0 only for simple.
    //   # Driver: row-major MBs, per MB [V-mb, V+4, V+8, V+12, H-mb,
    //   # H+4r, H+8r, H+12r] guarded by col>0 / inner / row>0 / inner
    //   # (dixie filter_row_normal / filter_row_simple).
    //   -----------------------------------------------------------------------

    /// Decode a (possibly newline-wrapped) hex string into bytes.
    fn unhex(s: &str) -> Vec<u8> {
        let hexed: String = s.chars().filter(char::is_ascii_hexdigit).collect();
        (0..hexed.len() / 2)
            .map(|i| u8::from_str_radix(&hexed[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    /// The shared 2x2-MB driver scene: `y = (x*7) ^ (y*13 + 37)`,
    /// `u = x*3 + y*5 + 11`, `v = x*5 + y*3 + 77` (all mod 256).
    fn scene() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (stride, uv_stride) = (32usize, 16usize);
        let mut y = vec![0u8; stride * 32];
        let mut u = vec![0u8; uv_stride * 16];
        let mut v = vec![0u8; uv_stride * 16];
        for ty in 0..32usize {
            for tx in 0..32usize {
                y[ty * stride + tx] = (((tx * 7) ^ (ty * 13 + 37)) & 0xFF) as u8;
            }
        }
        for ty in 0..16usize {
            for tx in 0..16usize {
                u[ty * uv_stride + tx] = ((tx * 3 + ty * 5 + 11) & 0xFF) as u8;
                v[ty * uv_stride + tx] = ((tx * 5 + ty * 3 + 77) & 0xFF) as u8;
            }
        }
        (y, u, v)
    }

    const NORMAL_Y: &str = "\
25222b3039060f141d1a6368717e474c55525ba0a9b6bf848d8a9398e1eef7fc32353c272e1118030a0d747f6669505b42454cb7bea1a8939a9d848ff6f9e0eb
3f38312a231c150e070079726b645d564f4841bab3aca59e97908982fbf4ede64c4b4259506f667d74730a0118172e253c3b32c9c0dfd6ede4e3faf188879e95
595e574c457a736861661f140d023b30292e27dcd5cac3f8f1f6efe49d928b80666168737a454c575e59202b323d040f161118e3eaf5fcc7cec9d0dba2adb4bf
73747d666f5059424b4c353e2728111a03040df6ffe0e9d2dbdcc5ceb7b8a1aa80878e959ca3aab1b8bfc6cdd4dbe2e9f0f7fe050c131a21282f363d444b5259
8d8a839891aea7bcb5b2cbc0d9d6efe4fdfaf308011e172c25223b3049465f549a9d948f86b9b0aba2a5dcd7cec1f8f3eaede41f1609003b32352c275e514843
a7a0a9b2bb848d969f98e1eaf3fcc5ced7d0d9222b343d060f08111a636c757eb4b3baa1a8979e858c8bf2f9e0efd6ddc4c3ca3138272e151c1b0209707f666d
c1c6cfd4dde2ebf0f9fe878c959aa3a8b1b6bf444d525b60696e777c050a1318cec9c0dbd2ede4fff6f188839a95aca7beb9b04b425d546f666178730a051c17
dbdcd5cec7f8f1eae3e49d968f80b9b2abaca55e5748417a73746d661f100902e8efe6fdf4cbc2d9d0d7aea5bcb38a81989f966d647b724940475e552c233a31
f5f2fbe0e9d6dfc4cdcab3b8a1ae979c85828b7079666f545d5a4348313e272c02050c171e2128333a3d444f5659606b72757c878e9198a3aaadb4bfc6c9d0db
0f08011a132c253e373049425b546d667f78718a839c95aea7a0b9b2cbc4ddd61c1b1209003f362d24235a5148477e756c6b6299908f86bdb4b3aaa1d8d7cec5
292e273c350a031811166f647d724b40595e57aca5bab38881869f94ede2fbf0363138232a151c070e09707b626d545f464148b3baa5ac979e99808bf2fde4ef
43444d565f6069727b7c050e1718212a33343dc6cfd0d9e2ebecf5fe8788919a50575e454c737a61686f161d040b323920272ed5dcc3caf1f8ffe6ed949b8289
5d5a5348417e776c65621b1009063f342d2a23d8d1cec7fcf5f2ebe099968f846a6d647f7649405b52552c273e3108031a1d14efe6f9f0cbc2c5dcd7aea1b8b3
777079626b545d464f48313a232c151e070009f2fbe4edd6dfd8c1cab3bca5ae84838a9198a7aeb5bcbbc2c9d0dfe6edf4f3fa0108171e252c2b3239404f565d
91969f848db2bba0a9aed7dcc5caf3f8e1e6ef141d020b30393e272c555a43489e99908b82bdb4afa6a1d8d3cac5fcf7eee9e01b120d043f363128235a554c47
abaca5beb788819a9394ede6fff0c9c2dbdcd52e2738310a03041d166f607972b8bfb6ada49b92898087fef5ece3dad1c8cfc63d342b221910170e057c736a61";

    const NORMAL_U: &str = "\
0b0e1114171a1d202326292c2f323538101316191c1f2225282b2e3134373a3d15181b1e2124272a2d303336393c3f421a1d202326292c2f3235383b3e414447
1f2225282b2e3134373a3d404346494c24272a2d303336393c3f4245484b4e51292c2f3235383b3e4144474a4d5053562e3134373a3d404346494c4f5255585b
3336393c3f4245484b4e5154575a5d60383b3e4144474a4d505356595c5f62653d404346494c4f5255585b5e6164676a4245484b4e5154575a5d606366696c6f
474a4d505356595c5f6265686b6e71744c4f5255585b5e6164676a6d707376795154575a5d606366696c6f7275787b7e56595c5f6265686b6e7174777a7d8083";

    const NORMAL_V: &str = "\
4d52575c61666b70757a7f84898e939850555a5f64696e73787d82878c91969b53585d62676c71767b80858a8f94999e565b60656a6f74797e83888d92979ca1
595e63686d72777c81868b90959a9fa45c61666b70757a7f84898e93989da2a75f64696e73787d82878c91969ba0a5aa62676c71767b80858a8f94999ea3a8ad
656a6f74797e83888d92979ca1a6abb0686d72777c81868b90959a9fa4a9aeb36b70757a7f84898e93989da2a7acb1b66e73787d82878c91969ba0a5aaafb4b9
71767b80858a8f94999ea3a8adb2b7bc74797e83888d92979ca1a6abb0b5babf777c81868b90959a9fa4a9aeb3b8bdc27a7f84898e93989da2a7acb1b6bbc0c5";

    const SIMPLE_Y: &str = "\
25222b3831060f161b1a6368717e474e53525ba0a9b6bf8e838a9398e1eef7fc32353c2f26111807060d74776e6950534a454cafc6a1a897969d848ff6f9e0eb
3f38312a231c150e070079726b645d564f4841bab3aca59e97908982fbf4ede64c4d4250596e647879730a0816172e343a3d33cbc0dfd6e4eae3edf188949e95
595c57454c7b756762661f1507023b27252c26dad5cac3f3f9f6fce49d858b806661687a73454c585d59202a333d0410151118e3eaf5fcd0c5c9d0dba2adb4bf
73747d6f66505947464c35372e2811130a040df6ffe0e9d7d6dcc5c7beb8a1aa8083879096a39eb1adbfc6cdd4dbe2e9f0f7fe050c131a21282f363d4f4b5259
8d8e8a959faeb3b8c4b2cbc8d1d6efecf5faf308011e172829223b3836465f549a9d94878eb9b0a9a4a5dcd7cec1f8f1ecede41f160900313c352c275e514843
a7a0a9bab3848d989d98e1eaf3fcc5d0d5d0d9222b343d100508111a636c757eb4b5bfa8a1979e8a878bf2f0e9efd6d6cbc1cb333d272e1a171b0209707f666d
c1c4cad5dce2ebf1f8fe878d949aa3a9b0b8be4248525b61686e777c050a1318cec9c0d2dbede4fafbf1888a9395acaeb7b9b04b425d546a6b6178730a051c17
dbdcd5c7cef8f1e9e4e49d978e80b9b1acaca55e574841717c746d661f100902e8efe6fdf4cbc2d3d6d7bbb7b7b68a87979f976c627b664a3e475e552c233a31
f5f2fbe8e1d6dfcac7caa6a6a6ab979686828a7973667b5b575a4340393e272c02050c171e2128333a3d444f5659606b72757c878e9198a3aaadb4bfc6c9d0db
0f0801121b2c253a3b30494a53546d6e777871828b9c95aaaba0b9bac3c4ddd61c1d1200093f362c26235a5049477e746d6d6390988f86b4bdb3ada2d8d4cec5
292c273d340a031315166f6d74724b47525c56aca6bab38b7e869c93ede5fbf03631382a23151c0c090970726b6d54584d4148b3baa5ac9c9999808bf2fde4ef
43444d575e6069737a7c050f1618212b32343dc6cfd0d9e3eaecf5fe8788919a5053573e487a7e6b696f130f07063a3c292b2fd1d5c2c6f2ffffebed91968284
5d5e5a474d77736660621e160e0b3731242622dcd8cfcbeff7f2e6e09c9b8f896a6d647f7649405558552c2f36310809141d14efe6f9f0cdc0c5dcd7aea1b8b3
7770796a63545d4c494831322b2c15180d0009f2fbe4eddcd9d8c1c2bbbca5ae84858f898fa7aeb6bbbbc2c8d1dfe6eef3f3fa0108171e262b2b3238414f565d
91949a8c96b2bba5a4aed7d5cccaf3f1e8e6ef141d020b2d3c3e272c555a43489e9990828bbdb4aea7a1d8d2cbc5fcf6efe9e01b120d04363f3128235a554c47
abaca5bfb68881959894edeff6f0c9c9d4dcd52e2738310d00041d166f607972b8bfb6ada49b92878287fef5ece3dacfcacfc63d342b221712170e057c736a61";

    /// Table sanity: lengths plus entries hand-derived from libvpx
    /// `vp8_loop_filter_update_sharpness` (vp8/common/vp8_loopfilter.c
    /// L49-75): inside = lvl >> (s>0) >> (s>4), capped at 9-s when s>0,
    /// min 1; lim=inside, blim=2*lvl+inside, mblim=2*(lvl+2)+inside.
    #[test]
    fn sharpness_tables_match_libvpx() {
        let t0 = LoopFilterTables::update_sharpness(0);
        assert_eq!(t0.lim.len(), 64);
        assert_eq!(t0.blim.len(), 64);
        assert_eq!(t0.mblim.len(), 64);
        // sharpness 0: lvl 0 -> (1, 1, 5), lvl 1 -> (1, 3, 7),
        // lvl 15 -> (15, 45, 49), lvl 63 -> (63, 189, 193).
        assert_eq!((t0.lim[0], t0.blim[0], t0.mblim[0]), (1, 1, 5));
        assert_eq!((t0.lim[1], t0.blim[1], t0.mblim[1]), (1, 3, 7));
        assert_eq!((t0.lim[15], t0.blim[15], t0.mblim[15]), (15, 45, 49));
        assert_eq!((t0.lim[63], t0.blim[63], t0.mblim[63]), (63, 189, 193));
        // sharpness 5: inside = lvl >> 2 capped at 4:
        // lvl 3 -> (1, 7, 11), lvl 20 -> (4, 44, 48), lvl 63 -> (4, 130, 134).
        let t5 = LoopFilterTables::update_sharpness(5);
        assert_eq!((t5.lim[3], t5.blim[3], t5.mblim[3]), (1, 7, 11));
        assert_eq!((t5.lim[20], t5.blim[20], t5.mblim[20]), (4, 44, 48));
        assert_eq!((t5.lim[63], t5.blim[63], t5.mblim[63]), (4, 130, 134));
        // sharpness 7: inside capped at 2:
        // lvl 4 -> (1, 9, 13), lvl 20 -> (2, 42, 46).
        let t7 = LoopFilterTables::update_sharpness(7);
        assert_eq!((t7.lim[4], t7.blim[4], t7.mblim[4]), (1, 9, 13));
        assert_eq!((t7.lim[20], t7.blim[20], t7.mblim[20]), (2, 42, 46));
        // The standalone helper must agree with the table for every level.
        for sharpness in 0..=7u8 {
            let t = LoopFilterTables::update_sharpness(sharpness);
            for level in 0..=63u8 {
                assert_eq!(
                    interior_limit(level, sharpness),
                    i32::from(t.lim[level as usize])
                );
            }
        }
    }

    /// §15.4 hev table (libvpx `lf_init_lut`): key 0/1/1/2 and inter
    /// 0/1/2/3 at levels 0/15/20/40 respectively.
    #[test]
    fn hev_threshold_table() {
        for level in [0u8, 14, 15, 19, 20, 39, 40, 63] {
            let l = i32::from(level);
            let want_key = if l >= 40 {
                2
            } else if l >= 15 {
                1
            } else {
                0
            };
            let want_inter = if l >= 40 {
                3
            } else if l >= 20 {
                2
            } else if l >= 15 {
                1
            } else {
                0
            };
            assert_eq!(hev_threshold(level, true), want_key);
            assert_eq!(hev_threshold(level, false), want_inter);
        }
        assert_eq!(hev_threshold(39, true), 1);
        assert_eq!(hev_threshold(39, false), 2);
    }

    /// Level 0 disables all filtering: a noisy frame is byte-identical.
    #[test]
    fn level_zero_disables_filtering() {
        let (y0, u0, v0) = scene();
        let mut y = y0.clone();
        let mut u = u0.clone();
        let mut v = v0.clone();
        filter_frame(
            &mut y,
            &mut u,
            &mut v,
            32,
            16,
            2,
            2,
            1,
            false,
            &[MbFilterInfo {
                level: 0,
                inner_edges: true,
            }; 4],
        );
        assert_eq!(y, y0);
        assert_eq!(u, u0);
        assert_eq!(v, v0);
    }

    /// Kernel: normal filter, vertical MB edge, non-hev wide path.
    /// Level 30, sharpness 0 -> interior 30, mblim = 2*32+30 = 94, hev = 2.
    /// Patch: cols < 16 = 110, cols >= 16 = 130. Python model row:
    /// w = sat_i8(-20 + 3*20) = 40; a27 = (1080+63)>>7 = 8, a18 = 6,
    /// a9 = 3 -> [110, 113, 116, 118 | 122, 124, 127, 130] at cols 12..19.
    #[test]
    fn mb_v_edge_normal_filter_kernel() {
        let mut y = vec![0u8; 32 * 16];
        for r in 0..16 {
            for c in 0..32 {
                y[r * 32 + c] = if c < 16 { 110 } else { 130 };
            }
        }
        let mut u = vec![128u8; 16 * 8];
        let mut v = vec![128u8; 16 * 8];
        filter_frame(
            &mut y,
            &mut u,
            &mut v,
            32,
            16,
            2,
            1,
            0,
            false,
            &[MbFilterInfo {
                level: 30,
                inner_edges: false,
            }; 2],
        );
        for r in 0..16 {
            assert_eq!(
                &y[r * 32 + 12..r * 32 + 20],
                &[110, 113, 116, 118, 122, 124, 127, 130],
                "row {r}"
            );
        }
        // Flat chroma planes are untouched (all filter deltas are 0).
        assert!(u.iter().all(|&b| b == 128));
        assert!(v.iter().all(|&b| b == 128));
    }

    /// Kernel: hev edge takes the simple ±f path (p1/q1 unchanged).
    /// Level 15, sharpness 0 -> interior 15, mblim = 2*17+15 = 49, hev = 1.
    /// |p1-p0| = 2 > 1 triggers hev; Python model: a = 3*16 - 10 = 38,
    /// f1 = 42>>3 = 5, f2 = 41>>3 = 5.
    #[test]
    fn hev_edge_takes_short_path() {
        let mut plane = vec![0u8; 8 * 8];
        for r in 0..8 {
            plane[r * 8..r * 8 + 8].copy_from_slice(&[101, 101, 102, 100, 116, 112, 112, 113]);
        }
        // Anchor: q0 at (row 0, col 4); 8 rows, one segment.
        filter_mb_v_edge(
            &mut plane,
            8,
            4,
            2 * (15 + 2) + 15,
            15,
            hev_threshold(15, false),
            1,
        );
        assert_eq!(&plane[0..8], &[101, 101, 102, 105, 111, 112, 112, 113]);
        // Outer-tap path leaves the inner taps alone.
        assert_eq!(plane[8 + 2], 102);
        assert_eq!(plane[8 + 5], 112);
    }

    /// Kernel: non-hev subblock edge runs the full path incl. p1/q1.
    /// Level 15, sharpness 0 -> interior 15, blim = 2*15+15 = 45.
    /// Python model: a = 3*10 = 30, f1 = 34>>3 = 4, f2 = 33>>3 = 4,
    /// inner a = (4+1)>>1 = 2.
    #[test]
    fn subblock_edge_takes_full_three_tap_path() {
        let mut plane = vec![0u8; 8 * 8];
        for r in 0..8 {
            plane[r * 8..r * 8 + 8].copy_from_slice(&[100, 100, 100, 100, 110, 111, 111, 111]);
        }
        filter_subblock_v_edge(
            &mut plane,
            8,
            4,
            2 * 15 + 15,
            15,
            hev_threshold(15, false),
            1,
        );
        assert_eq!(&plane[0..8], &[100, 100, 102, 104, 106, 109, 111, 111]);
    }

    /// Simple filter: MB edge gets f = 5 (mb_limit = 94), subblock edges
    /// are filtered too (no-op on flat interiors), chroma is untouched.
    #[test]
    fn simple_filter_leaves_chroma_untouched() {
        let mut y = vec![0u8; 32 * 16];
        for r in 0..16 {
            for c in 0..32 {
                y[r * 32 + c] = if c < 16 { 110 } else { 130 };
            }
        }
        let mut u = vec![0u8; 16 * 8];
        let mut v = vec![0u8; 16 * 8];
        for r in 0..8 {
            for c in 0..16 {
                u[r * 16 + c] = if c < 8 { 100 } else { 150 };
                v[r * 16 + c] = if c < 8 { 90 } else { 170 };
            }
        }
        let u_orig = u.clone();
        let v_orig = v.clone();
        filter_frame(
            &mut y,
            &mut u,
            &mut v,
            32,
            16,
            2,
            1,
            0,
            true,
            &[MbFilterInfo {
                level: 30,
                inner_edges: true,
            }; 2],
        );
        // a = 3*20 - 20 = 40 -> f1 = 44>>3 = 5, f2 = 43>>3 = 5.
        for r in 0..16 {
            assert_eq!(
                &y[r * 32 + 12..r * 32 + 20],
                &[110, 110, 110, 115, 125, 130, 130, 130],
                "row {r}"
            );
        }
        assert_eq!(u, u_orig);
        assert_eq!(v, v_orig);
    }

    /// Full-frame driver, normal filter: 2x2 MBs, level 25, sharpness 1,
    /// inner edges on. The whole frame must equal the Python model output
    /// byte-for-byte. Sample (17,17) is q1 of MB(1,1)'s V edge and then q1
    /// of its H edge — the match proves the dixie visit order
    /// (V-MB, subblock-V, H-MB, subblock-H) is reproduced.
    #[test]
    fn driver_normal_filter_frame_order() {
        let (y0, u0, v0) = scene();
        let mut y = y0;
        let mut u = u0;
        let mut v = v0;
        filter_frame_ex(
            &mut y,
            &mut u,
            &mut v,
            32,
            16,
            2,
            2,
            1,
            false,
            &[MbFilterInfo {
                level: 25,
                inner_edges: true,
            }; 4],
            false,
        );
        assert_eq!(y, unhex(NORMAL_Y));
        assert_eq!(u, unhex(NORMAL_U));
        assert_eq!(v, unhex(NORMAL_V));
        assert_eq!(y[17 * 32 + 17], 0x75, "order-probe pixel (17,17)");
    }

    /// Full-frame driver, simple filter: same scene, Y-only output.
    #[test]
    fn driver_simple_filter_frame() {
        let (_, u0, v0) = scene();
        let (mut y, _, _) = scene();
        let mut u = u0.clone();
        let mut v = v0.clone();
        filter_frame(
            &mut y,
            &mut u,
            &mut v,
            32,
            16,
            2,
            2,
            1,
            true,
            &[MbFilterInfo {
                level: 25,
                inner_edges: true,
            }; 4],
        );
        assert_eq!(y, unhex(SIMPLE_Y));
        assert_eq!(u, u0);
        assert_eq!(v, v0);
    }

    /// Frame type matters at level 25 (hev 2 inter / 1 key): a near-tap
    /// difference of exactly 2 takes the wide MBfilter path on inter
    /// frames and the hev ±f path on key frames. Python model rows:
    /// inter 6e6f71706f707071, key 6e6e706f70717171.
    #[test]
    fn frame_type_changes_hev_path() {
        let build = || {
            let mut y = vec![0u8; 32 * 16];
            for r in 0..16 {
                for c in 0..32 {
                    y[r * 32 + c] = if c == 14 {
                        112
                    } else if c < 16 {
                        110
                    } else {
                        113
                    };
                }
            }
            y
        };
        let mut uy = vec![128u8; 16 * 8];
        let mut vy = vec![128u8; 16 * 8];
        let mut ky = build();
        let mut uk = uy.clone();
        let mut vk = vy.clone();
        let info = [MbFilterInfo {
            level: 25,
            inner_edges: false,
        }; 2];
        let mut inter = build();
        filter_frame_ex(
            &mut inter, &mut uy, &mut vy, 32, 16, 2, 1, 1, false, &info, false,
        );
        filter_frame_ex(
            &mut ky, &mut uk, &mut vk, 32, 16, 2, 1, 1, false, &info, true,
        );
        assert_eq!(&inter[12..20], &[110, 111, 113, 112, 111, 112, 112, 113]);
        assert_eq!(&ky[12..20], &[110, 110, 112, 111, 112, 113, 113, 113]);
    }
}

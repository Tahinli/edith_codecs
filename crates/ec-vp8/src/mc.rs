//! Motion-compensation inter predictors: reference border extension, 6-tap
//! luma prediction, bilinear chroma prediction, and the UMV-border motion
//! vector clamps.
//!
//! Normative source: [RFC 6386] §18.3 (sub-pixel interpolation, two
//! separable 1-D passes, each sample `clamp255((a + 64) >> 7)`). The loop
//! structure, intermediate widths and the border clamps follow the libvpx
//! reference implementation (`vp8/common/filter.c`, `vp8/common/
//! reconinter.c`, `vp8/common/extend.c`) sample-for-sample; the per-pass
//! 0..=255 clamping of the 6-tap intermediate is mandated by the RFC's
//! `interp()` and is what the integer test vectors below pin down.
//!
//! Reference planes arrive already border-extended by [`extend_plane`]
//! (edge replication, `border` = 32 like libvpx's `VP8BORDERINPIXELS`);
//! `src` starts at the first byte of the bordered buffer (top-left border
//! corner) and visible pixel `(px, py)` therefore lives at
//! `src[(py + border) * stride + (px + border)]`.
//!
//! Motion vectors are `(row, col)` pairs in eighth-pel units (`i16`), and
//! the C arithmetic right shifts / two's-complement `& 7` phase extraction
//! are reproduced exactly (`i32 >> 3` and `& 7` on negatives).
//!
//! [RFC 6386]: https://www.rfc-editor.org/rfc/rfc6386

/// `VP8_FILTER_WEIGHT` (filter.c): tap weights sum to this, DC passes.
const VP8_FILTER_WEIGHT: i32 = 128;
/// `VP8_FILTER_SHIFT` (filter.c): normalize the 7-bit-precision result.
const VP8_FILTER_SHIFT: i32 = 7;

/// `vp8_sub_pel_filters[8][6]` (filter.c), indexed by the 0..8 sub-pel phase.
const SUB_PEL_FILTERS: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],   // degenerate whole-pel (RFC 6386 §18.3 "filters[0]")
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

/// `vp8_bilinear_filters[8][2]` (filter.c); chroma prediction is always
/// bilinear (RFC 6386 §19.2).
const BILINEAR_FILTERS: [[i32; 2]; 8] = [
    [128, 0],
    [112, 16],
    [96, 32],
    [80, 48],
    [64, 64],
    [48, 80],
    [32, 96],
    [16, 112],
];

#[inline]
fn clamp255(v: i32) -> i32 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        v
    }
}

/// Extends the visible `vis_w` x `vis_h` region of `plane` into the `border`
/// ring on all four sides by edge replication: every out-of-image sample
/// becomes the nearest in-image sample.
///
/// Port of `copy_and_extend_plane` (extend.c) with `et = el = eb = er =
/// border` and `interleave_step = 1`, in place: first the left/right border
/// of every visible row is filled from the row's first/last column, then the
/// (already side-extended) top and bottom rows are copied into the top and
/// bottom borders — which gives the corners their corner pixels, matching
/// the C exactly.
pub(crate) fn extend_plane(plane: &mut [u8], stride: usize, vis_w: usize, vis_h: usize, border: usize) {
    debug_assert!(vis_w >= 1 && vis_h >= 1);
    debug_assert!(stride >= border * 2 + vis_w);

    let line = border + vis_w + border;

    // extend.c: "copy the left and right most columns out".
    for r in 0..vis_h {
        let base = (border + r) * stride + border;
        let first = plane[base];
        let last = plane[base + vis_w - 1];
        plane[base - border..base].fill(first);
        plane[base + vis_w..base + vis_w + border].fill(last);
    }

    // extend.c: "Now copy the top and bottom lines into each line of the
    // respective borders" — src rows are the already side-extended lines.
    let top = border * stride;
    let bottom = (border + vis_h - 1) * stride;
    for i in 0..border {
        plane.copy_within(top..top + line, (border - 1 - i) * stride);
        plane.copy_within(bottom..bottom + line, (border + vis_h + i) * stride);
    }
}

/// Luma predictor for one `bw` x `bh` block (`bw`,`bh` in 4..=16, multiples
/// of 4) whose visible-image top-left pixel is `(x, y)`.
///
/// Semantics of `vp8_build_inter_predictors_b` /
/// `vp8_build_inter16x16_predictors_mby` + `vp8_sixtap_predict*` (reconinter.c,
/// filter.c): full-pel start `(x + (mv.1 >> 3), y + (mv.0 >> 3))`, phases
/// `fx = mv.1 & 7`, `fy = mv.0 & 7`; a whole-pel MV is a plain copy,
/// otherwise the separable 6-tap filter runs horizontally into an `i32`
/// intermediate — clamped to 0..=255 per pass like libvpx's `int` `FData` —
/// then vertically, both passes computing `(dot + 64) >> 7` and clamping.
///
/// When `need_clamp` is set, the MV is first passed through
/// [`clamp_mv_to_umv_border`].
pub(crate) fn predict_luma(
    src: &[u8],
    stride: usize,
    border: usize,
    vis_w: usize,
    vis_h: usize,
    x: usize,
    y: usize,
    mv: (i16, i16),
    need_clamp: bool,
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
) {
    debug_assert!(bw >= 4 && bw <= 16 && bw % 4 == 0);
    debug_assert!(bh >= 4 && bh <= 16 && bh % 4 == 0);

    let mut mv = (i32::from(mv.0), i32::from(mv.1));
    if need_clamp {
        clamp_mv_to_umv_border(&mut mv, x >> 4, y >> 4, vis_w, vis_h);
    }

    let fx = (mv.1 & 7) as usize;
    let fy = (mv.0 & 7) as usize;
    let sx = border as i32 + x as i32 + (mv.1 >> 3);
    let sy = border as i32 + y as i32 + (mv.0 >> 3);
    // 6-tap reads reach 2 before / 3 after the start; the libvpx clamp
    // arithmetic keeps those inside the 32-px border.
    debug_assert!(sx >= 2 && sy >= 2, "reference read outside bordered plane");
    let (sx, sy) = (sx as usize, sy as usize);

    if fx == 0 && fy == 0 {
        // reconinter.c build_inter_predictors_b / vp8_copy_mem*.
        for r in 0..bh {
            let s = (sy + r) * stride + sx;
            dst[r * dst_stride..r * dst_stride + bw].copy_from_slice(&src[s..s + bw]);
        }
        return;
    }

    let hf = &SUB_PEL_FILTERS[fx];
    let vf = &SUB_PEL_FILTERS[fy];

    // filter.c filter_block2d_first_pass on `src - 2*stride`: `bh + 5`
    // horizontal lines (2 above the block, 3 below, for the vertical taps).
    // Phase 0 is the exact identity (`(p * 128 + 64) >> 7 == p`), so the
    // uniform loop reproduces the C for every phase combination.
    let mut tmp = [0i32; 21 * 16]; // C: int FData[21 * 24]
    for i in 0..bh + 5 {
        let row = (sy + i - 2) * stride + sx - 2;
        for j in 0..bw {
            let p = row + j;
            let acc = i32::from(src[p]) * hf[0]
                + i32::from(src[p + 1]) * hf[1]
                + i32::from(src[p + 2]) * hf[2]
                + i32::from(src[p + 3]) * hf[3]
                + i32::from(src[p + 4]) * hf[4]
                + i32::from(src[p + 5]) * hf[5]
                + (VP8_FILTER_WEIGHT >> 1); /* Rounding */
            tmp[i * bw + j] = clamp255(acc >> VP8_FILTER_SHIFT);
        }
    }

    // filter.c filter_block2d_second_pass on `FData + 2*bw`: taps read the
    // intermediate rows `r .. r + 5`.
    for r in 0..bh {
        for c in 0..bw {
            let acc = tmp[r * bw + c] * vf[0]
                + tmp[(r + 1) * bw + c] * vf[1]
                + tmp[(r + 2) * bw + c] * vf[2]
                + tmp[(r + 3) * bw + c] * vf[3]
                + tmp[(r + 4) * bw + c] * vf[4]
                + tmp[(r + 5) * bw + c] * vf[5]
                + (VP8_FILTER_WEIGHT >> 1); /* Rounding */
            dst[r * dst_stride + c] = clamp255(acc >> VP8_FILTER_SHIFT) as u8;
        }
    }
}

/// Chroma predictor for one `bw` x `bh` block whose visible-image top-left
/// chroma pixel is `(x, y)`.
///
/// Semantics of `vp8_build_inter16x16_predictors_mbuv` /
/// `build_4x4uvmvs` consumers + `vp8_bilinear_predict*` (reconinter.c,
/// filter.c): separable 2-tap bilinear, `vp8_bilinear_filters[8][2]`,
/// weight 128 shift 7, `(dot + 64) >> 7` per pass. `mv` is the chroma MV in
/// eighth-CHROMA-pel units (the caller derives it from luma per libvpx: sum
/// of the four luma subblock MVs, rounding division by 8 with C
/// truncation-toward-zero, `& fullpixel_mask`).
///
/// The bilinear dot products are provably in 0..=255 (taps are
/// non-negative and sum to 128 over 0..=255 inputs), which is why libvpx's
/// bilinear passes store `unsigned short` intermediates and skip the clamp;
/// the values equal the RFC 6386 §18.3 `interp()` pseudo-code (which clamps)
/// regardless.
///
/// When `need_clamp` is set, the MV is first passed through
/// [`clamp_uvmv_to_umv_border`].
pub(crate) fn predict_chroma(
    src: &[u8],
    uv_stride: usize,
    border: usize,
    vis_w: usize,
    vis_h: usize,
    x: usize,
    y: usize,
    mv: (i16, i16),
    need_clamp: bool,
    dst: &mut [u8],
    dst_stride: usize,
    bw: usize,
    bh: usize,
) {
    debug_assert!(bw >= 2 && bw <= 16 && bw % 2 == 0);
    debug_assert!(bh >= 2 && bh <= 16 && bh % 2 == 0);

    let mut mv = (i32::from(mv.0), i32::from(mv.1));
    if need_clamp {
        clamp_uvmv_to_umv_border(&mut mv, x >> 3, y >> 3, vis_w, vis_h);
    }

    let fx = (mv.1 & 7) as usize;
    let fy = (mv.0 & 7) as usize;
    let sx = border as i32 + x as i32 + (mv.1 >> 3);
    let sy = border as i32 + y as i32 + (mv.0 >> 3);
    debug_assert!(sx >= 0 && sy >= 0, "reference read outside bordered plane");
    let (sx, sy) = (sx as usize, sy as usize);

    if fx == 0 && fy == 0 {
        for r in 0..bh {
            let s = (sy + r) * uv_stride + sx;
            dst[r * dst_stride..r * dst_stride + bw].copy_from_slice(&src[s..s + bw]);
        }
        return;
    }

    let hf = &BILINEAR_FILTERS[fx];
    let vf = &BILINEAR_FILTERS[fy];

    // filter.c filter_block2d_bil_first_pass: `bh + 1` rows, horizontal
    // taps [0] and [1]. Phase 0 is the exact identity, as above.
    let mut tmp = [0u16; 17 * 16]; // C: unsigned short FData[17 * 16]
    for i in 0..bh + 1 {
        let row = (sy + i) * uv_stride + sx;
        for j in 0..bw {
            let p = row + j;
            let acc = i32::from(src[p]) * hf[0] + i32::from(src[p + 1]) * hf[1]
                + (VP8_FILTER_WEIGHT / 2); /* Rounding */
            tmp[i * bw + j] = (acc >> VP8_FILTER_SHIFT) as u16;
        }
    }

    // filter.c filter_block2d_bil_second_pass: vertical taps [0] and
    // `[width]` (the next intermediate row).
    for r in 0..bh {
        for c in 0..bw {
            let acc = i32::from(tmp[r * bw + c]) * vf[0]
                + i32::from(tmp[(r + 1) * bw + c]) * vf[1]
                + (VP8_FILTER_WEIGHT / 2); /* Rounding */
            dst[r * dst_stride + c] = (acc >> VP8_FILTER_SHIFT) as u8;
        }
    }
}

/// The macroblock-level UMV border edges in eighth-pel units, exactly the
/// decoder's `xd->mb_to_{left,right,top,bottom}_edge` (decodeframe.c:503-532):
///
/// ```c
/// xd->mb_to_top_edge    = -((mb_row * 16) << 3);
/// xd->mb_to_bottom_edge = ((mb_rows - 1 - mb_row) * 16) << 3;
/// xd->mb_to_left_edge   = -((mb_col * 16) << 3);
/// xd->mb_to_right_edge  = ((mb_cols - 1 - mb_col) * 16) << 3;
/// ```
///
/// with `mb_rows`/`mb_cols` the full (partial MB inclusive) MB counts of the
/// visible frame. Returns `(left, right, top, bottom)`.
fn mb_edges(mb_col: usize, mb_row: usize, vis_w: usize, vis_h: usize) -> (i32, i32, i32, i32) {
    let mb_cols = (vis_w + 15) >> 4;
    let mb_rows = (vis_h + 15) >> 4;
    debug_assert!(mb_col < mb_cols && mb_row < mb_rows);
    let left = -(((mb_col * 16) as i32) << 3);
    let right = (((mb_cols - 1 - mb_col) * 16) as i32) << 3;
    let top = -(((mb_row * 16) as i32) << 3);
    let bottom = (((mb_rows - 1 - mb_row) * 16) as i32) << 3;
    (left, right, top, bottom)
}

/// Port of `clamp_mv_to_umv_border` (reconinter.c:257-278), luma MV in
/// eighth-pel units:
///
/// ```c
/// if (mv->col < (xd->mb_to_left_edge - (19 << 3))) {
///   mv->col = xd->mb_to_left_edge - (16 << 3);
/// } else if (mv->col > xd->mb_to_right_edge + (18 << 3)) {
///   mv->col = xd->mb_to_right_edge + (16 << 3);
/// }
/// /* same for row with top/bottom edges */
/// ```
///
/// 19 px slack toward left/top (16 px + 3 taps right of the central pixel),
/// 18 px toward right/bottom (16 px + 2 taps left of it); the clamp anchor
/// is 16 px inside the border. `mb_col`/`mb_row` locate the block's
/// macroblock; libvpx clamps with these MB-level edges for whole-MB MVs and,
/// when `need_to_clamp_mvs` is set, for each SPLITMV subblock MV too
/// (reconinter.c:370-375, 397-400).
fn clamp_mv_to_umv_border(mv: &mut (i32, i32), mb_col: usize, mb_row: usize, vis_w: usize, vis_h: usize) {
    let (left, right, top, bottom) = mb_edges(mb_col, mb_row, vis_w, vis_h);
    if mv.1 < left - (19 << 3) {
        mv.1 = left - (16 << 3);
    } else if mv.1 > right + (18 << 3) {
        mv.1 = right + (16 << 3);
    }

    if mv.0 < top - (19 << 3) {
        mv.0 = top - (16 << 3);
    } else if mv.0 > bottom + (18 << 3) {
        mv.0 = bottom + (16 << 3);
    }
}

/// Port of `clamp_uvmv_to_umv_border` (reconinter.c:281-295), chroma MV in
/// eighth-CHROMA-pel units. The comparison doubles the chroma MV back into
/// luma eighth-pel units (`2 * mv->col`, the "different divisor": the
/// chroma border reach is half the luma one) and the clamp target is the
/// luma anchor halved:
///
/// ```c
/// mv->col = (2 * mv->col < (xd->mb_to_left_edge - (19 << 3)))
///               ? (xd->mb_to_left_edge - (16 << 3)) >> 1
///               : mv->col;
/// mv->col = (2 * mv->col > xd->mb_to_right_edge + (18 << 3))
///               ? (xd->mb_to_right_edge + (16 << 3)) >> 1
///               : mv->col;
/// /* same for row */
/// ```
///
/// The C applies the two ternaries in sequence (right check sees the
/// left-clamped value, which cannot trip it); the sequential `if`s here are
/// that verbatim. `(left - (16 << 3))` is a multiple of 256 so `>> 1` is
/// exact. Callers must re-apply this clamp AFTER full-pel rounding of a
/// derived chroma MV: rounding to full-pel can move the MV outside the
/// border tap window even though the luma MV was already clamped
/// (reconinter.c:336-339 — the 16x16 path clamps the chroma MV
/// unconditionally for exactly this reason).
fn clamp_uvmv_to_umv_border(mv: &mut (i32, i32), mb_col: usize, mb_row: usize, vis_w: usize, vis_h: usize) {
    let (left, right, top, bottom) = mb_edges(mb_col, mb_row, vis_w, vis_h);

    if 2 * mv.1 < left - (19 << 3) {
        mv.1 = (left - (16 << 3)) >> 1;
    }
    if 2 * mv.1 > right + (18 << 3) {
        mv.1 = (right + (16 << 3)) >> 1;
    }

    if 2 * mv.0 < top - (19 << 3) {
        mv.0 = (top - (16 << 3)) >> 1;
    }
    if 2 * mv.0 > bottom + (18 << 3) {
        mv.0 = (bottom + (16 << 3)) >> 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic plane fill shared by every test (LCG, same sequence in
    /// the hand-computed literals below).
    fn lcg_plane(seed: u32, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        let mut s = seed;
        for b in out.iter_mut() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (s >> 16) as u8;
        }
        out
    }

    const BORDER: usize = 32;
    const STRIDE: usize = 104; // 40 visible + 2 * 32 border
    const VIS_W: usize = 40; // 3 MB columns, last one partial
    const VIS_H: usize = 36; // 3 MB rows, last one partial
    const ROWS: usize = VIS_H + 2 * BORDER;

    fn plane() -> Vec<u8> {
        lcg_plane(0x1234, STRIDE * ROWS)
    }

    /// Reads visible-image pixel (`vx`, `vy`) out of a bordered plane.
    fn vis(plane: &[u8], vx: usize, vy: usize) -> u8 {
        plane[(vy + BORDER) * STRIDE + (vx + BORDER)]
    }

    #[test]
    fn extend_plane_replicates_edges_and_corners() {
        let mut p = plane();
        // Distinct visible values, sentinel everywhere else.
        for r in 0..VIS_H {
            for c in 0..VIS_W {
                p[(BORDER + r) * STRIDE + BORDER + c] = ((r * 7 + c * 3) % 251 + 4) as u8;
            }
        }
        p[..BORDER * STRIDE].fill(0xEE);
        p[(BORDER + VIS_H) * STRIDE..].fill(0xEE);
        for r in 0..VIS_H {
            let row = &mut p[(BORDER + r) * STRIDE..(BORDER + r) * STRIDE + STRIDE];
            row[..BORDER].fill(0xEE);
            row[BORDER + VIS_W..].fill(0xEE);
        }

        extend_plane(&mut p, STRIDE, VIS_W, VIS_H, BORDER);

        // Every sample of the bordered plane equals the nearest in-image sample.
        for r in 0..ROWS {
            for c in 0..STRIDE {
                let vr = r.saturating_sub(BORDER).min(VIS_H - 1);
                let vc = c.saturating_sub(BORDER).min(VIS_W - 1);
                assert_eq!(p[r * STRIDE + c], vis(&p, vc, vr), "sample ({r},{c})");
            }
        }
        // Explicit corners (worst case of the rule above).
        assert_eq!(p[0], vis(&p, 0, 0)); // top-left
        assert_eq!(p[line_len() - 1], vis(&p, VIS_W - 1, 0)); // top-right
        assert_eq!(p[(ROWS - 1) * STRIDE], vis(&p, 0, VIS_H - 1)); // bottom-left
        assert_eq!(
            p[ROWS * STRIDE - 1],
            vis(&p, VIS_W - 1, VIS_H - 1) // bottom-right
        );
        // The visible region itself is untouched.
        for r in 0..VIS_H {
            for c in 0..VIS_W {
                assert_eq!(
                    p[(BORDER + r) * STRIDE + BORDER + c],
                    ((r * 7 + c * 3) % 251 + 4) as u8
                );
            }
        }
    }

    fn line_len() -> usize {
        BORDER + VIS_W + BORDER
    }

    #[test]
    fn integer_mv_predict_is_a_plain_copy() {
        let p = plane();
        let mut dst = [0u8; 16 * 16];

        // Whole-pel luma MV (row=16, col=-8) → full-pel offset (x-1, y+2).
        // mv.0 is the row component, mv.1 the column component. x=y=8 keeps
        // every read inside the visible region of the unextended plane.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 8, 8, (16, -8), false, &mut dst, 16, 16, 16,
        );
        for r in 0..16 {
            for c in 0..16 {
                assert_eq!(dst[r * 16 + c], vis(&p, 8 - 1 + c, 8 + 2 + r), "({r},{c})");
            }
        }

        // Zero MV → identity copy.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 8, 8, (0, 0), false, &mut dst, 16, 16, 16,
        );
        for r in 0..16 {
            for c in 0..16 {
                assert_eq!(dst[r * 16 + c], vis(&p, 8 + c, 8 + r), "({r},{c})");
            }
        }

        // Chroma copy path, whole-pel chroma MV (row=-16, col=+8)
        // → full-pel offset (x+1, y-2).
        let mut dst8 = [0u8; 8 * 8];
        predict_chroma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 4, 4, (-16, 8), false, &mut dst8, 8, 8, 8,
        );
        for r in 0..8 {
            for c in 0..8 {
                assert_eq!(dst8[r * 8 + c], vis(&p, 4 + 1 + c, 4 - 2 + r), "({r},{c})");
            }
        }
    }

    #[test]
    fn sixtap_pure_horizontal_matches_hand_computed_values() {
        let p = plane();
        let mut dst = [0u8; 4 * 4];

        // mv = (0, 4): full-pel start (16, 12), fx = 4, fy = 0.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 16, 12, (0, 4), false, &mut dst, 4, 4, 4,
        );

        // Hand-computed from vp8_sub_pel_filters[4] = { 3, -16, 77, 77, -16, 3 }
        // and vp8_sub_pel_filters[0] = { 0, 0, 128, 0, 0, 0 } (vertical is the
        // exact identity over the clamped horizontal intermediate).
        // out[0][0]: taps are visible pixels (12, 14..19) = [183, 106, 2, 87, 75, 198]:
        //   183*3 - 106*16 + 2*77 + 87*77 - 75*16 + 198*3 + 64 = 5164
        //   5164 >> 7 = 40, already in 0..=255.
        // out[1][2]: taps (13, 17..22) = [153, 188, 219, 13, 86, 194]:
        //   153*3 - 188*16 + 219*77 + 13*77 - 86*16 + 194*3 + 64 = 14585
        //   14585 >> 7 = 113.
        assert_eq!(dst[0], 40);
        assert_eq!(dst[1 * 4 + 2], 113);

        // Full block against the tap-table reference formula.
        for r in 0..4 {
            for c in 0..4 {
                let taps: Vec<i32> = (0..6)
                    .map(|k| i32::from(vis(&p, 16 + c - 2 + k as usize, 12 + r)))
                    .collect();
                let acc: i32 = taps
                    .iter()
                    .zip(SUB_PEL_FILTERS[4])
                    .map(|(t, f)| t * f)
                    .sum::<i32>()
                    + 64;
                assert_eq!(
                    dst[r * 4 + c],
                    (acc >> 7).clamp(0, 255) as u8,
                    "out[{r}][{c}] taps={taps:?} acc={acc}"
                );
            }
        }
    }

    #[test]
    fn sixtap_combined_phase_16x16_matches_hand_computed_values() {
        let p = plane();
        let mut dst = [0u8; 16 * 16];

        // mv = (14, 18): full-pel start (10, 9), fx = 2, fy = 6.
        predict_luma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 8, 8, (14, 18), false, &mut dst, 16, 16, 16,
        );

        // vp8_sub_pel_filters[2] = { 2, -11, 108, 36, -8, 1 }, each
        // intermediate clamped to 0..=255 (e.g. tmp[3][10] taps
        // [75, 190, 18, 15, 104, 40], raw dot -184, -184 >> 7 = -2, clamped 0);
        // vertical pass with vp8_sub_pel_filters[6] = { 1, -8, 36, 108, -11, 2 }.
        //
        // out[0][0]: horizontal taps are visible pixels (7, 8..13), giving the
        //   post-clamp intermediate column h_tmp = [41, 186, 229, 210, 95, 185]:
        //   41*1 - 186*8 + 229*36 + 210*108 - 95*11 + 185*2 + 64 = 28866
        //   28866 >> 7 = 225, already in 0..=255.
        //
        // out[5][9]: horizontal taps are visible pixels (11, 15..20),
        //   h_tmp = [205, 8, 14, 189, 108, 4]:
        //   205*1 - 8*8 + 14*36 + 189*108 - 108*11 + 4*2 + 64 = 19941
        //   19941 >> 7 = 155.
        assert_eq!(dst[0], 225);
        assert_eq!(dst[5 * 16 + 9], 155);

        // Full block against the two-pass reference formula.
        let mut tmp = [0i32; 21 * 16];
        for i in 0..21 {
            for j in 0..16 {
                let acc: i32 = (0..6)
                    .map(|k| i32::from(vis(&p, 10 - 2 + j + k, 9 - 2 + i)))
                    .zip(SUB_PEL_FILTERS[2])
                    .map(|(t, f)| t * f)
                    .sum::<i32>()
                    + 64;
                tmp[i * 16 + j] = (acc >> 7).clamp(0, 255);
            }
        }
        for r in 0..16 {
            for c in 0..16 {
                let acc: i32 = (0..6)
                    .map(|k| tmp[(r + k) * 16 + c])
                    .zip(SUB_PEL_FILTERS[6])
                    .map(|(t, f)| t * f)
                    .sum::<i32>()
                    + 64;
                assert_eq!(
                    dst[r * 16 + c],
                    (acc >> 7).clamp(0, 255) as u8,
                    "out[{r}][{c}]"
                );
            }
        }
    }

    #[test]
    fn bilinear_chroma_matches_hand_computed_values() {
        let p = plane();
        let mut dst = [0u8; 8 * 8];

        // mv = (11, 5): full-pel start (4, 7), fx = 5, fy = 3.
        predict_chroma(
            &p, STRIDE, BORDER, VIS_W, VIS_H, 4, 6, (11, 5), false, &mut dst, 8, 8, 8,
        );

        // Horizontal taps { 48, 80 } (phase 5), vertical { 80, 48 } (phase 3),
        // both passes (dot + 64) >> 7, no clamp (provably in range).
        //
        // out[0][0]: tmp[0][0] = (160*48 + 96*80 + 64) >> 7 = 120 from visible
        //   pixels (4,7),(5,7); tmp[1][0] = (107*48 + 42*80 + 64) >> 7 = 66
        //   from (4,8),(5,8).
        //   120*80 + 66*48 + 64 = 12832, 12832 >> 7 = 100.
        // out[3][7]: tmp[3][7] = 84, tmp[4][7] = 189;
        //   84*80 + 189*48 + 64 = 15856, 15856 >> 7 = 123.
        assert_eq!(dst[0], 100);
        assert_eq!(dst[3 * 8 + 7], 123);

        // Full block against the two-pass reference formula.
        let mut tmp = [0u16; 9 * 8];
        for i in 0..9 {
            for j in 0..8 {
                tmp[i * 8 + j] = ((i32::from(vis(&p, 4 + j, 7 + i)) * 48
                    + i32::from(vis(&p, 4 + j + 1, 7 + i)) * 80
                    + 64)
                    >> 7) as u16;
            }
        }
        for r in 0..8 {
            for c in 0..8 {
                let acc = i32::from(tmp[r * 8 + c]) * 80 + i32::from(tmp[(r + 1) * 8 + c]) * 48 + 64;
                assert_eq!(dst[r * 8 + c], (acc >> 7) as u8, "out[{r}][{c}]");
            }
        }
    }

    #[test]
    fn umv_border_clamps_match_libvpx_arithmetic() {
        let p = plane();
        let mut a = [0u8; 16 * 16];
        let mut b = [0u8; 16 * 16];

        // Single-MB frame (vis 16x16): mb_to_left_edge = mb_to_top_edge = 0,
        // mb_to_right_edge = mb_to_bottom_edge = 0.
        //
        // Luma: mv (-1000, -1000) < 0 - 152 → both components set to -128
        // (left - (16 << 3)); prediction must equal an explicit (-128, -128).
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-1000, -1000), true, &mut a, 16, 16, 16,
        );
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-128, -128), false, &mut b, 16, 16, 16,
        );
        assert_eq!(a, b);

        // Strict threshold: (-152, -152) is NOT clamped (condition is strict
        // `<`), (-153, -153) IS clamped to (-128, -128).
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-152, -152), true, &mut a, 16, 16, 16,
        );
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-152, -152), false, &mut b, 16, 16, 16,
        );
        assert_eq!(a, b);
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-153, -153), true, &mut a, 16, 16, 16,
        );
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (-128, -128), false, &mut b, 16, 16, 16,
        );
        assert_eq!(a, b);

        // Right/bottom: mv (4000, 4000) > 0 + 144 → set to +128.
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (4000, 4000), true, &mut a, 16, 16, 16,
        );
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (128, 128), false, &mut b, 16, 16, 16,
        );
        assert_eq!(a, b);

        // Chroma: 2 * (-2000) < -152 → (-128 >> 1) = -64 per component.
        let mut ca = [0u8; 8 * 8];
        let mut cb = [0u8; 8 * 8];
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (-2000, -2000), true, &mut ca, 8, 8, 8,
        );
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (-64, -64), false, &mut cb, 8, 8, 8,
        );
        assert_eq!(ca, cb);

        // Chroma right/bottom: 2 * 2000 > 144 → (128 >> 1) = 64.
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (2000, 2000), true, &mut ca, 8, 8, 8,
        );
        predict_chroma(
            &p, STRIDE, BORDER, 8, 8, 0, 0, (64, 64), false, &mut cb, 8, 8, 8,
        );
        assert_eq!(ca, cb);

        // An in-range MV with need_clamp=true must pass through untouched.
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (10, -10), true, &mut a, 16, 16, 16,
        );
        predict_luma(
            &p, STRIDE, BORDER, 16, 16, 0, 0, (10, -10), false, &mut b, 16, 16, 16,
        );
        assert_eq!(a, b);
    }
}

//! CDEF constants and filter kernels. //! av1split: moved out of `decode/mod.rs`; the items are unchanged apart from visibility. (D14)

use super::*;

/// Spec 7.15.3's `Cdef_Directions`: `(row, col)` offsets for the two primary
/// taps of each of the eight directions; the secondary taps at `dir+2`/`dir-2`
/// (mod 8) reuse the same table.
pub(super) const CDEF_DIRECTIONS: [[(i32, i32); 2]; 8] = [
    [(-1, 1), (-2, 2)],
    [(0, 1), (-1, 2)],
    [(0, 1), (0, 2)],
    [(0, 1), (1, 2)],
    [(1, 1), (2, 2)],
    [(1, 0), (2, 1)],
    [(1, 0), (2, 0)],
    [(1, 0), (2, -1)],
];
/// A neighbour tap outside the frame's true extent -- `constrain` reads this
/// as "so far away it never contributes" (spec/libaom `CDEF_VERY_LARGE`).
pub(super) const CDEF_VERY_LARGE: i32 = 0x4000;
pub(super) const CDEF_PRI_TAPS: [[i32; 2]; 2] = [[4, 2], [3, 3]];
pub(super) const CDEF_SEC_TAPS: [i32; 2] = [2, 1];
/// lane-av1422filter: libaom `av1_cdef_filter_fb` (cdef_block.c) remaps the
/// luma direction before filtering CHROMA whenever `xdec != ydec` — the
/// direction grid lives on the subsampled chroma grid, so a direction found
/// on the 8x8 luma block means a different line there. `conv422` applies
/// under 4:2:2 (`xdec` set), `conv440` under its mirror (not expressible in
/// the profiles this decoder accepts, kept for fidelity). 4:2:0 and 4:4:4
/// (`xdec == ydec`) take no remap, exactly as before this lane.
pub(super) const CDEF_DIR_CONV422: [usize; 8] = [7, 0, 2, 4, 5, 6, 6, 6];
pub(super) const CDEF_DIR_CONV440: [usize; 8] = [1, 2, 2, 2, 3, 4, 6, 0];

fn cdef_msb(x: i32) -> i32 {
    31 - x.leading_zeros() as i32
}

/// Spec 7.15.3's `constrain`: how much of a neighbour's difference from the
/// centre sample a filter tap is allowed to contribute.
#[allow(dead_code)] // read only from the `#[cfg(test)]` gates
pub(super) fn cdef_constrain(diff: i32, threshold: i32, damping: i32) -> i32 {
    if threshold == 0 {
        return 0;
    }
    cdef_constrain_shifted(diff, threshold, cdef_constrain_shift(threshold, damping))
}

/// `constrain`'s damping shift: a function of the block's threshold and
/// damping alone, so the filter loop computes it once per block instead of
/// the `leading_zeros` + branch it used to do twice per tap read.
pub(super) fn cdef_constrain_shift(threshold: i32, damping: i32) -> i32 {
    (damping - cdef_msb(threshold)).max(0)
}

/// [`cdef_constrain`]'s body with the shift already resolved; `threshold` is
/// non-zero on every call (the caller only filters when its strength is).
pub(super) fn cdef_constrain_shifted(diff: i32, threshold: i32, shift: i32) -> i32 {
    let a = diff.abs();
    diff.signum() * (threshold - (a >> shift)).clamp(0, a)
}

/// The widest `CDEF_DIRECTIONS` offset in either axis: every tap a block reads
/// lies inside its own samples grown by this much.
pub(super) const CDEF_BORDER: usize = 2;

/// Copies one block's whole CDEF footprint -- its `bh` x `bw` samples plus the
/// [`CDEF_BORDER`] tap ring -- out of the *unfiltered* plane into `buf` (row
/// stride `bw + 2 * CDEF_BORDER`), so the direction search and the tap loops
/// index a small contiguous window instead of re-deriving the frame-bounds
/// test on every one of a pixel's ~17 tap reads. Out-of-frame positions get
/// `CDEF_VERY_LARGE`, exactly as the per-sample closure did.
// Signature mirrors the spec's per-call inputs; a parameter struct would only move them.
#[allow(clippy::too_many_arguments)]
pub(super) fn cdef_gather(
    src: &[u16],
    stride: usize,
    true_w: usize,
    true_h: usize,
    ox: usize,
    oy: usize,
    bw: usize,
    bh: usize,
    buf: &mut [i16],
) {
    let bstride = bw + 2 * CDEF_BORDER;
    let rows = bh + 2 * CDEF_BORDER;
    if oy >= CDEF_BORDER
        && ox >= CDEF_BORDER
        && oy + bh + CDEF_BORDER <= true_h
        && ox + bw + CDEF_BORDER <= true_w
    {
        // Interior block: every tap is inside the frame, so the whole window
        // is `rows` contiguous runs.
        for r in 0..rows {
            let s0 = (oy - CDEF_BORDER + r) * stride + (ox - CDEF_BORDER);
            let src_row = &src[s0..s0 + bstride];
            let dst_row = &mut buf[r * bstride..r * bstride + bstride];
            for (d, &v) in dst_row.iter_mut().zip(src_row) {
                *d = v as i16;
            }
        }
        return;
    }
    for r in 0..rows {
        let ny = oy as isize + r as isize - CDEF_BORDER as isize;
        for c in 0..bstride {
            let nx = ox as isize + c as isize - CDEF_BORDER as isize;
            buf[r * bstride + c] =
                if ny < 0 || nx < 0 || ny as usize >= true_h || nx as usize >= true_w {
                    CDEF_VERY_LARGE as i16
                } else {
                    src[ny as usize * stride + nx as usize] as i16
                };
        }
    }
}

/// libaom `cdef_find_dir_c`'s eight partial-sum arrays, row at a time: each
/// array takes the row, its reverse, its pairwise sums or its total added
/// into consecutive slots, so the 512 scalar read-modify-writes on i64 the
/// per-sample form did become a handful of lane-wide adds per row. i32 is
/// exact here: every partial is a sum of at most eight values in -128..127,
/// and slot 15 exists only so an eight-lane store at offset 7 fits.
pub(super) fn cdef_find_dir_partials(win: &[i16], bstride: usize, coeff_shift: i32) -> [[i32; 16]; 8] {
    fn add_at<const N: usize>(dst: &mut [i32; 16], off: usize, src: &[i32; N]) {
        for (d, &x) in dst[off..off + N].iter_mut().zip(src.iter()) {
            *d += x;
        }
    }
    let mut partial = [[0i32; 16]; 8];
    for i in 0..8usize {
        let base = (i + CDEF_BORDER) * bstride + CDEF_BORDER;
        // libaom `cdef_find_dir_c`: normalise to an 8-bit sample before
        // accumulating, same as `constrain`'s threshold shift.
        let row: [i32; 8] =
            core::array::from_fn(|j| (i32::from(win[base + j]) >> coeff_shift) - 128);
        let rev: [i32; 8] = core::array::from_fn(|j| row[7 - j]);
        let pair: [i32; 4] = core::array::from_fn(|k| row[2 * k] + row[2 * k + 1]);
        let rpair: [i32; 4] = core::array::from_fn(|k| pair[3 - k]);
        let (p, rest) = partial.split_at_mut(1);
        add_at(&mut p[0], i, &row); // partial[0][i + j]
        add_at(&mut rest[0], i, &pair); // partial[1][i + j / 2]
        rest[1][i] = row.iter().sum(); // partial[2][i]
        add_at(&mut rest[2], i, &rpair); // partial[3][3 + i - j / 2]
        add_at(&mut rest[3], i, &rev); // partial[4][7 + i - j]
        add_at(&mut rest[4], 3 - i / 2, &row); // partial[5][3 - i / 2 + j]
        add_at(&mut rest[5], 0, &row); // partial[6][j]
        add_at(&mut rest[6], i / 2, &row); // partial[7][i / 2 + j]
    }
    partial
}

/// Spec 7.15.3's direction search (libaom `cdef_find_dir_c`): the dominant
/// direction of an 8x8 luma window and the variance gap between it and its
/// orthogonal, reading [`cdef_gather`]'s window (row stride `bstride`, the
/// block's own samples starting at `CDEF_BORDER, CDEF_BORDER`).
/// Runtime dispatch of the direction search's partial sums: the AVX2 clone
/// runs [`cdef_simd::find_dir_partials_avx2`], the baseline clone the scalar
/// form. Both read the same eight rows of eight samples.
#[multiversion::multiversion(targets("x86_64+avx2"))]
pub(super) fn cdef_find_dir_partials_dispatch(win: &[i16], bstride: usize, coeff_shift: i32) -> [[i32; 16]; 8] {
    multiversion::target::match_target! {
        "x86_64+avx2" => cdef_simd::find_dir_partials_avx2(win, bstride, coeff_shift),
        _ => cdef_find_dir_partials(win, bstride, coeff_shift),
    }
}

pub(super) fn cdef_find_dir(win: &[i16], bstride: usize, coeff_shift: i32) -> (usize, i32) {
    const DIV_TABLE: [i64; 9] = [0, 840, 420, 280, 210, 168, 140, 120, 105];
    let partial = cdef_find_dir_partials_dispatch(win, bstride, coeff_shift);
    let sq = |a: i32| -> i64 { i64::from(a) * i64::from(a) };
    let mut cost = [0i64; 8];
    // Index loop mirrors the spec's per-position form over several offsets; an iterator would obscure it.
    #[allow(clippy::needless_range_loop)]
    for i in 0..8usize {
        cost[2] += sq(partial[2][i]);
        cost[6] += sq(partial[6][i]);
    }
    cost[2] *= DIV_TABLE[8];
    cost[6] *= DIV_TABLE[8];
    for i in 0..7usize {
        cost[0] += (sq(partial[0][i]) + sq(partial[0][14 - i])) * DIV_TABLE[i + 1];
        cost[4] += (sq(partial[4][i]) + sq(partial[4][14 - i])) * DIV_TABLE[i + 1];
    }
    cost[0] += sq(partial[0][7]) * DIV_TABLE[8];
    cost[4] += sq(partial[4][7]) * DIV_TABLE[8];
    for i in (1..8).step_by(2) {
        for j in 0..5usize {
            cost[i] += sq(partial[i][3 + j]);
        }
        cost[i] *= DIV_TABLE[8];
        for j in 0..3usize {
            cost[i] += (sq(partial[i][j]) + sq(partial[i][10 - j])) * DIV_TABLE[2 * j + 2];
        }
    }
    let mut best_cost = 0i64;
    let mut best_dir = 0usize;
    for (i, &c) in cost.iter().enumerate() {
        if c > best_cost {
            best_cost = c;
            best_dir = i;
        }
    }
    let var = (best_cost - cost[(best_dir + 4) & 7]) >> 10;
    (best_dir, var as i32)
}

/// Spec 7.15.2's per-8x8 strength adjustment: a strongly directional 8x8
/// (high variance gap) gets more deringing than a flat or non-directional
/// one, only ever applied to the luma primary strength.
pub(super) fn cdef_adjust_strength(strength: i32, var: i32) -> i32 {
    if var == 0 {
        return 0;
    }
    let i = if var >> 6 != 0 {
        cdef_msb(var >> 6).min(12)
    } else {
        0
    };
    (strength * (4 + i) + 8) >> 4
}

/// Spec 7.15.3's `cdef_filter_block`: filters one block (8x8 luma, 4x4 4:2:0
/// chroma) in place, `sample(row, col)` reading the *unfiltered* frame (a
/// previously-filtered neighbour must never feed this block's own sum).
#[allow(clippy::too_many_arguments)]
#[multiversion::multiversion(targets("x86_64+avx2"))]
pub(super) fn cdef_filter_block(
    src: &[i16],
    dst: &mut [u16],
    stride: usize,
    ox: usize,
    oy: usize,
    bw: usize,
    bh: usize,
    pri_strength: i32,
    sec_strength: i32,
    dir: usize,
    pri_damping: i32,
    sec_damping: i32,
    enable_primary: bool,
    enable_secondary: bool,
    coeff_shift: i32,
    fctx: &crate::decode::FrameCtx,
) {
    let smax = sample_max(fctx);
    multiversion::target::match_target! {
        "x86_64+avx2" => {
            if (bw == 4 || bw == 8) && bh.is_multiple_of(2) {
                // The kernel reads `src`'s whole (bw + 4) x (bh + 4) window
                // and writes exactly the `bh` runs of `bw` samples the
                // scalar form writes.
                cdef_simd::filter_block_avx2(
                    src,
                    dst,
                    stride,
                    ox,
                    oy,
                    bw,
                    bh,
                    pri_strength,
                    sec_strength,
                    dir,
                    pri_damping,
                    sec_damping,
                    enable_primary,
                    enable_secondary,
                    coeff_shift,
                    smax,
                );
                return;
            }
        }
        _ => {}
    }
    cdef_filter_block_scalar(
        src,
        dst,
        stride,
        ox,
        oy,
        bw,
        bh,
        pri_strength,
        sec_strength,
        dir,
        pri_damping,
        sec_damping,
        enable_primary,
        enable_secondary,
        coeff_shift,
        smax,
    );
}

/// [`cdef_filter_block`]'s scalar reference: spec 7.15.3 sample by sample,
/// the form every non-AVX2 build decodes through and the one
/// `cdef_simd_kernel_matches_the_scalar_filter` checks the kernel against.
#[allow(clippy::too_many_arguments)]
pub(super) fn cdef_filter_block_scalar(
    src: &[i16],
    dst: &mut [u16],
    stride: usize,
    ox: usize,
    oy: usize,
    bw: usize,
    bh: usize,
    pri_strength: i32,
    sec_strength: i32,
    dir: usize,
    pri_damping: i32,
    sec_damping: i32,
    enable_primary: bool,
    enable_secondary: bool,
    coeff_shift: i32,
    smax: i32,
) {
    let clipping_required = enable_primary && enable_secondary;
    // libaom `cdef_filter_block_internal`: `cdef_pri_taps[(pri_strength >>
    // coeff_shift) & 1]` -- `pri_strength` (and its `adjust_strength`
    // derivative) carries the bit-depth shift, so the tap-set parity bit
    // must be read back above that shift, not off the raw low bit.
    let pri_taps = CDEF_PRI_TAPS[((pri_strength >> coeff_shift) & 1) as usize];
    // Both damping shifts depend only on this block's strengths (spec 7.15.3's
    // `constrain`), so they are resolved here rather than per tap read.
    let pri_shift = cdef_constrain_shift(pri_strength.max(1), pri_damping);
    let sec_shift = cdef_constrain_shift(sec_strength.max(1), sec_damping);
    let bstride = bw + 2 * CDEF_BORDER;
    // `src` is [`cdef_gather`]'s window: (i, j) in block coordinates, taps
    // reaching +-CDEF_BORDER, so every index below is in range by construction.
    let at = |i: i32, j: i32| -> i32 {
        i32::from(
            src[(i + CDEF_BORDER as i32) as usize * bstride + (j + CDEF_BORDER as i32) as usize],
        )
    };
    let (pri_dir, sec_dir0, sec_dir1) = (
        CDEF_DIRECTIONS[dir],
        CDEF_DIRECTIONS[(dir + 2) & 7],
        CDEF_DIRECTIONS[(dir + 6) & 7],
    );
    for i in 0..bh {
        for j in 0..bw {
            let (i32i, i32j) = (i as i32, j as i32);
            let x = at(i32i, i32j);
            let mut sum = 0i32;
            let mut max = x;
            let mut min = x;
            for k in 0..2usize {
                if enable_primary {
                    let (tr, tc) = pri_dir[k];
                    let p0 = at(i32i + tr, i32j + tc);
                    let p1 = at(i32i - tr, i32j - tc);
                    sum += pri_taps[k] * cdef_constrain_shifted(p0 - x, pri_strength, pri_shift);
                    sum += pri_taps[k] * cdef_constrain_shifted(p1 - x, pri_strength, pri_shift);
                    if clipping_required {
                        if p0 != CDEF_VERY_LARGE {
                            max = max.max(p0);
                        }
                        if p1 != CDEF_VERY_LARGE {
                            max = max.max(p1);
                        }
                        min = min.min(p0).min(p1);
                    }
                }
                if enable_secondary {
                    let (tr0, tc0) = sec_dir0[k];
                    let (tr1, tc1) = sec_dir1[k];
                    let s0 = at(i32i + tr0, i32j + tc0);
                    let s1 = at(i32i - tr0, i32j - tc0);
                    let s2 = at(i32i + tr1, i32j + tc1);
                    let s3 = at(i32i - tr1, i32j - tc1);
                    if clipping_required {
                        for s in [s0, s1, s2, s3] {
                            if s != CDEF_VERY_LARGE {
                                max = max.max(s);
                            }
                        }
                        min = min.min(s0).min(s1).min(s2).min(s3);
                    }
                    sum += CDEF_SEC_TAPS[k]
                        * (cdef_constrain_shifted(s0 - x, sec_strength, sec_shift)
                            + cdef_constrain_shifted(s1 - x, sec_strength, sec_shift)
                            + cdef_constrain_shifted(s2 - x, sec_strength, sec_shift)
                            + cdef_constrain_shifted(s3 - x, sec_strength, sec_shift));
                }
            }
            let mut y = x + ((8 + sum - i32::from(sum < 0)) >> 4);
            if clipping_required {
                y = y.clamp(min, max);
            }
            dst[(oy + i) * stride + (ox + j)] = y.clamp(0, smax) as u16;
        }
    }
}

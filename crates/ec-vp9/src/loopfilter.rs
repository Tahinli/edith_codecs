//! Loop filter for keyframes (spec 8.8; libvpx `vpx_dsp/loopfilter.c`
//! kernels + `vp9_loopfilter.c` edge selection, ported verbatim).
//!
//! Rather than libvpx's per-superblock LOOP_FILTER_MASK bitmaps, the
//! pass re-derives the same per-edge decisions from decode-time grids
//! (per-4x4 luma tx sizes, per-8x8 block map, block transform sizes and
//! levels). The rules `build_masks`/`build_y_mask` encode:
//!
//! - an internal 4x4 edge is filtered (4-tap) iff its tx size is 4x4;
//! - a prediction-block edge uses 16 taps for 16x16/32x32 transforms,
//!   8 taps for 8x8, and 4 taps for 4x4 — promoted to 8 taps on
//!   32x32 (luma) / 64x64 (chroma) borders;
//! - chroma edges exist for >=16x16 blocks (pred edges at the block's
//!   own left/top sized by the uv transform, internal 4x4 uv edges when
//!   the uv transform is 4x4) and for 8x8-region blocks only on the
//!   z-(0,0) slot of each 16x16 area (4-tap, 8-tap at the 64-luma
//!   border);
//! - intra skip blocks filter like any other (the early return in
//!   `build_masks` is gated on `is_inter_block`).

use crate::tables::*;

/// `vp9_loopfilter.c` UV mask tables (BLOCK_SIZES indexed 4X4..64X64).
const UV_LEFT_PRED: [u16; 13] = [
    0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0011, 0x0001, 0x0011, 0x1111, 0x0011,
    0x1111,
];
const UV_ABOVE_PRED: [u16; 13] = [
    0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0003, 0x0003, 0x0003, 0x000f,
    0x000f,
];
const UV_SIZE_MASK: [u16; 13] = [
    0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0001, 0x0011, 0x0003, 0x0033, 0x3333, 0x00ff,
    0xffff,
];
const UV_LEFT_TX: [u16; 4] = [0xffff, 0xffff, 0x5555, 0x1111];
const UV_ABOVE_TX: [u16; 4] = [0xffff, 0xffff, 0x0f0f, 0x000f];

/// `vp9_loopfilter.c` luma mask tables (BLOCK_SIZES indexed 4X4..64X64).
const LUMA_LEFT_PRED: [u64; 13] = [
    0x1,
    0x1,
    0x1,
    0x1,
    0x101,
    0x1,
    0x101,
    0x1010101,
    0x101,
    0x1010101,
    0x0101010101010101,
    0x1010101,
    0x0101010101010101,
];
const LUMA_ABOVE_PRED: [u64; 13] = [
    0x1, 0x1, 0x1, 0x1, 0x1, 0x3, 0x3, 0x3, 0xf, 0xf, 0xf, 0xff, 0xff,
];
const LUMA_SIZE_MASK: [u64; 13] = [
    0x1,
    0x1,
    0x1,
    0x1,
    0x101,
    0x3,
    0x303,
    0x3030303,
    0xf0f,
    0xf0f0f0f,
    0x0f0f0f0f0f0f0f0f,
    0xffffffff,
    0xffffffffffffffff,
];
const LUMA_LEFT_TXF: [u64; 4] = [u64::MAX, u64::MAX, 0x5555555555555555, 0x1111111111111111];
const LUMA_ABOVE_TXF: [u64; 4] = [u64::MAX, u64::MAX, 0x00ff00ff00ff00ff, 0x000000ff000000ff];
const LUMA_LEFT_BORDER: u64 = 0x1111111111111111;
const LUMA_ABOVE_BORDER: u64 = 0x000000ff000000ff;

#[inline]
fn sc(x: i32) -> i32 {
    x.clamp(-128, 127)
}

/// `filter4` kernel; `idx` = [op1, op0, oq0, oq1].
/// `vpx_dsp filter4`: updates p1/p0/q0/q1 in place. `mask` gates the
/// whole filter, `hev` the outer taps (vp9_loopfilter.c:81).
fn filter4(s: &mut [u8], idx: [usize; 4], thresh: i32, mask: bool) {
    if !mask {
        return;
    }
    let (op1, op0, oq0, oq1) = (idx[0], idx[1], idx[2], idx[3]);
    let (p1, p0, q0, q1) = (s[op1], s[op0], s[oq0], s[oq1]);
    let hev = (p1 as i32 - p0 as i32).abs() > thresh || (q1 as i32 - q0 as i32).abs() > thresh;
    let (ps1, ps0, qs0, qs1) = (
        (p1 ^ 0x80) as i8 as i32,
        (p0 ^ 0x80) as i8 as i32,
        (q0 ^ 0x80) as i8 as i32,
        (q1 ^ 0x80) as i8 as i32,
    );
    let mut filter = sc(ps1 - qs1);
    if hev {
        // add outer taps if we have high edge variance (& hev mask)
    } else {
        filter = 0;
    }
    // inner taps
    filter = sc(filter + 3 * (qs0 - ps0));
    // round one side +4 and the other +3
    let filter1 = sc(filter + 4) >> 3;
    let filter2 = sc(filter + 3) >> 3;
    s[oq0] = (sc(qs0 - filter1) ^ 0x80) as u8;
    s[op0] = (sc(ps0 + filter2) ^ 0x80) as u8;
    // outer tap adjustments (& ~hev)
    let f = if hev { 0 } else { (filter1 + 1) >> 1 };
    s[oq1] = (sc(qs1 - f) ^ 0x80) as u8;
    s[op1] = (sc(ps1 + f) ^ 0x80) as u8;
}

fn lpf_edge(
    data: &mut [u8],
    off: usize,
    stride: usize,
    dir: u8,
    taps: u8,
    blimit: i32,
    limit: i32,
    thresh: i32,
    nlines: isize,
) {
    // `dir` 0 = horizontal edges (rows step by stride), 1 = vertical
    // (columns step by 1). `taps` in {4, 8, 16}.
    let step: isize = if dir == 0 { stride as isize } else { 1 };
    let line_step: isize = if dir == 0 { 1 } else { stride as isize };
    // luma: 4 lines per 4x4; chroma ss11: 8 lines matching vpx_lpf_*_c.
    for line in 0..nlines {
        let edge = off as isize + line * line_step;
        macro_rules! rd {
            ($k:expr) => {
                data[(edge + $k * step) as usize] as i32
            };
        }
        macro_rules! put {
            ($k:expr, $v:expr) => {
                data[(edge + $k * step) as usize] = $v;
            };
        }
        // filter_mask (vpx_dsp/loopfilter.c:34): p3..q3 gradient limits +
        // blimit; shared by the 4-, 8- and 16-tap entry points.
        let mask = (rd!(-4) - rd!(-3)).abs() <= limit
            && (rd!(-3) - rd!(-2)).abs() <= limit
            && (rd!(-2) - rd!(-1)).abs() <= limit
            && (rd!(1) - rd!(0)).abs() <= limit
            && (rd!(2) - rd!(1)).abs() <= limit
            && (rd!(3) - rd!(2)).abs() <= limit
            && 2 * (rd!(-1) - rd!(0)).abs() + (rd!(-2) - rd!(1)).abs() / 2 <= blimit;
        let idx4 = [
            (edge - 2 * step) as usize,
            (edge - step) as usize,
            edge as usize,
            (edge + step) as usize,
        ];
        if taps == 4 {
            filter4(data, idx4, thresh, mask);
            continue;
        }
        if !mask {
            continue;
        }
        // flat_mask4(1, p3..q3)
        let flat = (rd!(-2) - rd!(-1)).abs() <= 1
            && (rd!(1) - rd!(0)).abs() <= 1
            && (rd!(-3) - rd!(-1)).abs() <= 1
            && (rd!(2) - rd!(0)).abs() <= 1
            && (rd!(-4) - rd!(-1)).abs() <= 1
            && (rd!(3) - rd!(0)).abs() <= 1;
        if taps == 8 || !flat {
            if !flat {
                // plain filter4 with the real mask and hev threshold
                filter4(data, idx4, thresh, mask);
                continue;
            }
            // 7-tap [1,1,1,2,1,1,1] outputs (filter8).
            let (p3, p2, p1, p0, q0, q1, q2, q3) = (
                rd!(-4),
                rd!(-3),
                rd!(-2),
                rd!(-1),
                rd!(0),
                rd!(1),
                rd!(2),
                rd!(3),
            );
            put!(-3, ((p3 * 3 + p2 * 2 + p1 + p0 + q0 + 4) >> 3) as u8);
            put!(-2, ((p3 * 2 + p2 + p1 * 2 + p0 + q0 + q1 + 4) >> 3) as u8);
            put!(-1, ((p3 + p2 + p1 + p0 * 2 + q0 + q1 + q2 + 4) >> 3) as u8);
            put!(0, ((p2 + p1 + p0 + q0 * 2 + q1 + q2 + q3 + 4) >> 3) as u8);
            put!(1, ((p1 + p0 + q0 + q1 * 2 + q2 + q3 + q3 + 4) >> 3) as u8);
            put!(2, ((p0 + q0 + q1 + q2 * 2 + q3 + q3 + q3 + 4) >> 3) as u8);
            continue;
        }
        // flat_mask5(1, s[-8], s[-7], s[-6], s[-5], p0, q0, s[4], s[5], s[6], s[7]):
        // |p7..p4 - p0| and |q4..q7 - q0| (the C names are shifted; q4..q6
        // are the q1..q3 args of the inner flat_mask4 and compare to q0).
        let flat2 = (rd!(-8) - rd!(-1)).abs() <= 1
            && (rd!(-7) - rd!(-1)).abs() <= 1
            && (rd!(-6) - rd!(-1)).abs() <= 1
            && (rd!(-5) - rd!(-1)).abs() <= 1
            && (rd!(4) - rd!(0)).abs() <= 1
            && (rd!(5) - rd!(0)).abs() <= 1
            && (rd!(6) - rd!(0)).abs() <= 1
            && (rd!(7) - rd!(0)).abs() <= 1;
        if !flat2 {
            let (p3, p2, p1, p0, q0, q1, q2, q3) = (
                rd!(-4),
                rd!(-3),
                rd!(-2),
                rd!(-1),
                rd!(0),
                rd!(1),
                rd!(2),
                rd!(3),
            );
            put!(-3, ((p3 * 3 + p2 * 2 + p1 + p0 + q0 + 4) >> 3) as u8);
            put!(-2, ((p3 * 2 + p2 + p1 * 2 + p0 + q0 + q1 + 4) >> 3) as u8);
            put!(-1, ((p3 + p2 + p1 + p0 * 2 + q0 + q1 + q2 + 4) >> 3) as u8);
            put!(0, ((p2 + p1 + p0 + q0 * 2 + q1 + q2 + q3 + 4) >> 3) as u8);
            put!(1, ((p1 + p0 + q0 + q1 * 2 + q2 + q3 + q3 + 4) >> 3) as u8);
            put!(2, ((p0 + q0 + q1 + q2 * 2 + q3 + q3 + q3 + 4) >> 3) as u8);
            continue;
        }
        // 15-tap filter [1 x 7, 2, 1 x 7] (filter16, weights verbatim).
        let (p7, p6, p5, p4) = (rd!(-8), rd!(-7), rd!(-6), rd!(-5));
        let (p3, p2, p1, p0) = (rd!(-4), rd!(-3), rd!(-2), rd!(-1));
        let (q0, q1, q2, q3) = (rd!(0), rd!(1), rd!(2), rd!(3));
        let (q4, q5, q6, q7) = (rd!(4), rd!(5), rd!(6), rd!(7));
        put!(
            -7,
            ((p7 * 7 + p6 * 2 + p5 + p4 + p3 + p2 + p1 + p0 + q0 + 8) >> 4) as u8
        );
        put!(
            -6,
            ((p7 * 6 + p6 + p5 * 2 + p4 + p3 + p2 + p1 + p0 + q0 + q1 + 8) >> 4) as u8
        );
        put!(
            -5,
            ((p7 * 5 + p6 + p5 + p4 * 2 + p3 + p2 + p1 + p0 + q0 + q1 + q2 + 8) >> 4) as u8
        );
        put!(
            -4,
            ((p7 * 4 + p6 + p5 + p4 + p3 * 2 + p2 + p1 + p0 + q0 + q1 + q2 + q3 + 8) >> 4) as u8
        );
        put!(
            -3,
            ((p7 * 3 + p6 + p5 + p4 + p3 + p2 * 2 + p1 + p0 + q0 + q1 + q2 + q3 + q4 + 8) >> 4)
                as u8
        );
        put!(
            -2,
            ((p7 * 2 + p6 + p5 + p4 + p3 + p2 + p1 * 2 + p0 + q0 + q1 + q2 + q3 + q4 + q5 + 8) >> 4)
                as u8
        );
        put!(
            -1,
            ((p7 + p6 + p5 + p4 + p3 + p2 + p1 + p0 * 2 + q0 + q1 + q2 + q3 + q4 + q5 + q6 + 8)
                >> 4) as u8
        );
        put!(
            0,
            ((p6 + p5 + p4 + p3 + p2 + p1 + p0 + q0 * 2 + q1 + q2 + q3 + q4 + q5 + q6 + q7 + 8)
                >> 4) as u8
        );
        put!(
            1,
            ((p5 + p4 + p3 + p2 + p1 + p0 + q0 + q1 * 2 + q2 + q3 + q4 + q5 + q6 + q7 * 2 + 8) >> 4)
                as u8
        );
        put!(
            2,
            ((p4 + p3 + p2 + p1 + p0 + q0 + q1 + q2 * 2 + q3 + q4 + q5 + q6 + q7 * 3 + 8) >> 4)
                as u8
        );
        put!(
            3,
            ((p3 + p2 + p1 + p0 + q0 + q1 + q2 + q3 * 2 + q4 + q5 + q6 + q7 * 4 + 8) >> 4) as u8
        );
        put!(
            4,
            ((p2 + p1 + p0 + q0 + q1 + q2 + q3 + q4 * 2 + q5 + q6 + q7 * 5 + 8) >> 4) as u8
        );
        put!(
            5,
            ((p1 + p0 + q0 + q1 + q2 + q3 + q4 + q5 * 2 + q6 + q7 * 6 + 8) >> 4) as u8
        );
        put!(
            6,
            ((p0 + q0 + q1 + q2 + q3 + q4 + q5 + q6 * 2 + q7 * 7 + 8) >> 4) as u8
        );
    }
}

/// Everything the edge pass needs, recorded during decode.
pub(crate) struct LfGrids {
    /// Per-8x8 filter level (last covering block wins, decode order).
    pub level8: Vec<u8>,
    /// Per-8x8 owning block: (bsize, origin mi_row, origin mi_col).
    pub blk: Vec<(u8, u32, u32)>,
    /// Per-8x8 BLOCK-level tx size (sub-8x8 blocks record TX_4X4).
    pub otx: Vec<u8>,
    pub mi_cols: usize,
    pub mi_rows: usize,
}

/// limits for a level (spec 8.8.1, `update_sharpness` +
/// `vp9_loop_filter_init`): (mblim, lim, hev_thr).
fn limits(level: u8, sharpness: u8) -> (i32, i32, i32) {
    let mut bil = level as i32 >> ((usize::from(sharpness > 0)) + (usize::from(sharpness > 4)));
    if sharpness > 0 && bil > 9 - sharpness as i32 {
        bil = 9 - sharpness as i32;
    }
    if bil < 1 {
        bil = 1;
    }
    (2 * (level as i32 + 2) + bil, bil, level as i32 >> 4)
}

impl LfGrids {
    /// `build_masks`/`build_y_mask` luma contribution of one block at its
    /// position (they are identical for luma).
    fn add_luma_masks(
        &self,
        r: usize,
        c: usize,
        left: &mut [u64; 4],
        above: &mut [u64; 4],
        int4: &mut u64,
    ) {
        let cell = r * self.mi_cols + c;
        if self.level8[cell] == 0 {
            return;
        }
        let b = self.blk[cell].0 as usize;
        let tx = self.otx[cell] as usize;
        let sh = (c & 7) + ((r & 7) << 3);
        left[tx] |= LUMA_LEFT_PRED[b] << sh;
        above[tx] |= LUMA_ABOVE_PRED[b] << sh;
        left[tx] |= (LUMA_SIZE_MASK[b] & LUMA_LEFT_TXF[tx]) << sh;
        above[tx] |= (LUMA_SIZE_MASK[b] & LUMA_ABOVE_TXF[tx]) << sh;
        if tx == TX_4X4 {
            *int4 |= LUMA_SIZE_MASK[b] << sh;
        }
    }

    /// `vp9_setup_mask`'s traversal + `vp9_adjust_mask` for the luma plane
    /// of one 64x64 superblock.
    fn luma_masks(&self, sbr: usize, sbc: usize) -> ([u64; 4], [u64; 4], u64) {
        let mut left = [0u64; 4];
        let mut above = [0u64; 4];
        let mut int4 = 0u64;
        let max_rows = (sbr + 8).min(self.mi_rows) - sbr;
        let max_cols = (sbc + 8).min(self.mi_cols) - sbc;
        let bsize = |r: usize, c: usize| self.blk[r * self.mi_cols + c].0 as usize;
        match bsize(sbr, sbc) {
            12 => self.add_luma_masks(sbr, sbc, &mut left, &mut above, &mut int4),
            11 => {
                self.add_luma_masks(sbr, sbc, &mut left, &mut above, &mut int4);
                if 4 < max_rows {
                    self.add_luma_masks(sbr + 4, sbc, &mut left, &mut above, &mut int4);
                }
            }
            10 => {
                self.add_luma_masks(sbr, sbc, &mut left, &mut above, &mut int4);
                if 4 < max_cols {
                    self.add_luma_masks(sbr, sbc + 4, &mut left, &mut above, &mut int4);
                }
            }
            _ => {
                for i32 in 0..4usize {
                    let ro32 = (i32 >> 1) * 4;
                    let co32 = (i32 & 1) * 4;
                    if co32 >= max_cols || ro32 >= max_rows {
                        continue;
                    }
                    let r32 = sbr + ro32;
                    let c32 = sbc + co32;
                    match bsize(r32, c32) {
                        9 => self.add_luma_masks(r32, c32, &mut left, &mut above, &mut int4),
                        8 => {
                            self.add_luma_masks(r32, c32, &mut left, &mut above, &mut int4);
                            if ro32 + 2 < max_rows {
                                self.add_luma_masks(r32 + 2, c32, &mut left, &mut above, &mut int4);
                            }
                        }
                        7 => {
                            self.add_luma_masks(r32, c32, &mut left, &mut above, &mut int4);
                            if co32 + 2 < max_cols {
                                self.add_luma_masks(r32, c32 + 2, &mut left, &mut above, &mut int4);
                            }
                        }
                        _ => {
                            for i16 in 0..4usize {
                                let ro16 = ro32 + (i16 >> 1) * 2;
                                let co16 = co32 + (i16 & 1) * 2;
                                if co16 >= max_cols || ro16 >= max_rows {
                                    continue;
                                }
                                let r16 = sbr + ro16;
                                let c16 = sbc + co16;
                                match bsize(r16, c16) {
                                    6 => {
                                        self.add_luma_masks(
                                            r16, c16, &mut left, &mut above, &mut int4,
                                        );
                                    }
                                    5 => {
                                        self.add_luma_masks(
                                            r16, c16, &mut left, &mut above, &mut int4,
                                        );
                                        if ro16 + 1 < max_rows {
                                            self.add_luma_masks(
                                                r16 + 1,
                                                c16,
                                                &mut left,
                                                &mut above,
                                                &mut int4,
                                            );
                                        }
                                    }
                                    4 => {
                                        self.add_luma_masks(
                                            r16, c16, &mut left, &mut above, &mut int4,
                                        );
                                        if co16 + 1 < max_cols {
                                            self.add_luma_masks(
                                                r16,
                                                c16 + 1,
                                                &mut left,
                                                &mut above,
                                                &mut int4,
                                            );
                                        }
                                    }
                                    _ => {
                                        self.add_luma_masks(
                                            r16, c16, &mut left, &mut above, &mut int4,
                                        );
                                        for i8 in 1..4usize {
                                            if co16 + (i8 & 1) >= max_cols
                                                || ro16 + (i8 >> 1) >= max_rows
                                            {
                                                continue;
                                            }
                                            self.add_luma_masks(
                                                r16 + (i8 >> 1),
                                                c16 + (i8 & 1),
                                                &mut left,
                                                &mut above,
                                                &mut int4,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // vp9_adjust_mask.
        left[TX_16X16] |= left[TX_32X32];
        above[TX_16X16] |= above[TX_32X32];
        left[TX_8X8] |= left[TX_4X4] & LUMA_LEFT_BORDER;
        left[TX_4X4] &= !LUMA_LEFT_BORDER;
        above[TX_8X8] |= above[TX_4X4] & LUMA_ABOVE_BORDER;
        above[TX_4X4] &= !LUMA_ABOVE_BORDER;
        if sbr + 8 > self.mi_rows {
            let rows = self.mi_rows - sbr;
            let mask_y = ((1u64 << (rows * 8)) as u64).wrapping_sub(1);
            for i in 0..3 {
                left[i] &= mask_y;
                above[i] &= mask_y;
            }
            int4 &= mask_y;
        }
        if sbc + 8 > self.mi_cols {
            let columns = self.mi_cols - sbc;
            let mask_y = ((1u64 << columns) as u64)
                .wrapping_sub(1)
                .wrapping_mul(0x0101010101010101);
            for i in 0..3 {
                left[i] &= mask_y;
                above[i] &= mask_y;
            }
            int4 &= mask_y;
        }
        if sbc == 0 {
            for i in 0..3 {
                left[i] &= 0xfefefefefefefefe;
            }
        }
        (left, above, int4)
    }

    /// Filter the frame's three planes in place. `y_w/h`, `uv_w/h` are
    /// the coded (aligned) plane sizes.
    pub(crate) fn filter_frame(
        &self,
        y: &mut [u8],
        stride: usize,
        u: &mut [u8],
        v: &mut [u8],
        uv_stride: usize,
        y_w: usize,
        y_h: usize,
        uv_w: usize,
        uv_h: usize,
        sharpness: u8,
    ) {
        // Luma masks, ported from vp9_setup_mask/build_masks and
        // vp9_adjust_mask (bit i = MI row i>>3, MI col i&7; each bit
        // covers an 8x8 cell). Edges are emitted from the masks.
        let sb_rows = (self.mi_rows + 7) / 8;
        let sb_cols = (self.mi_cols + 7) / 8;
        for sbr_i in 0..sb_rows {
            let sbr = sbr_i * 8;
            for sbc_i in 0..sb_cols {
                let sbc = sbc_i * 8;
                let (left, above, int4) = self.luma_masks(sbr, sbc);
                for row in 0..8usize {
                    let yy = sbr * 8 + row * 8;
                    if yy >= y_h {
                        break;
                    }
                    for col in 0..8usize {
                        let xx = sbc * 8 + col * 8;
                        if xx >= y_w {
                            break;
                        }
                        let bit = 1u64 << (row * 8 + col);
                        let lvl = self.level8[(sbr + row) * self.mi_cols + sbc + col];
                        if lvl == 0 {
                            continue;
                        }
                        let (bl, li, th) = limits(lvl, sharpness);
                        // Vertical edge at xx, spanning rows yy..yy+7.
                        let mut t = if left[TX_16X16] & bit != 0 {
                            16
                        } else if left[TX_8X8] & bit != 0 {
                            8
                        } else if left[TX_4X4] & bit != 0 {
                            4
                        } else {
                            0
                        };
                        if t != 0 && xx >= 4 && xx + 4 <= y_w {
                            if t == 16 && (xx < 8 || xx + 8 > y_w) {
                                t = 8;
                            }
                            lpf_edge(y, yy * stride + xx, stride, 1, t, bl, li, th, 8);
                            if std::env::var_os("LFTRACE").is_some() {
                                let kind = match t {
                                    16 => "V16",
                                    8 => "V8",
                                    _ => "V4",
                                };
                                eprintln!("{kind} {} {}", xx, yy);
                            }
                        }
                        if int4 & bit != 0 && xx + 8 <= y_w && yy + 8 <= y_h {
                            lpf_edge(y, yy * stride + xx + 4, stride, 1, 4, bl, li, th, 8);
                            if std::env::var_os("LFTRACE").is_some() {
                                eprintln!("V4I {} {}", xx + 4, yy);
                            }
                        }
                    }
                }
                for row in 0..8usize {
                    let yy = sbr * 8 + row * 8;
                    if yy >= y_h {
                        break;
                    }
                    for col in 0..8usize {
                        let xx = sbc * 8 + col * 8;
                        if xx >= y_w {
                            break;
                        }
                        let bit = 1u64 << (row * 8 + col);
                        let lvl = self.level8[(sbr + row) * self.mi_cols + sbc + col];
                        if lvl == 0 {
                            continue;
                        }
                        let (bl, li, th) = limits(lvl, sharpness);
                        // Horizontal edge at yy, spanning cols xx..xx+7.
                        if yy > 0 {
                            let mut t = if above[TX_16X16] & bit != 0 {
                                16
                            } else if above[TX_8X8] & bit != 0 {
                                8
                            } else if above[TX_4X4] & bit != 0 {
                                4
                            } else {
                                0
                            };
                            if t != 0 && yy >= 4 && yy + 4 <= y_h && xx + 8 <= y_w {
                                if t == 16 && (yy < 8 || yy + 8 > y_h) {
                                    t = 8;
                                }
                                lpf_edge(y, yy * stride + xx, stride, 0, t, bl, li, th, 8);
                                if std::env::var_os("LFTRACE").is_some() {
                                    let kind = match t {
                                        16 => "H16",
                                        8 => "H8",
                                        _ => "H4",
                                    };
                                    eprintln!("{kind} {} {}", xx, yy);
                                }
                            }
                        }
                        // Internal 4x4 UV edge at yy + 4 (skipped on the
                        // frame's last MI row: `skip_border_4x4_r`).
                        if int4 & bit != 0
                            && sbr + row != self.mi_rows - 1
                            && yy + 8 <= y_h
                            && xx + 8 <= y_w
                        {
                            lpf_edge(y, (yy + 4) * stride + xx, stride, 0, 4, bl, li, th, 8);
                            if std::env::var_os("LFTRACE").is_some() {
                                eprintln!("H4I {} {}", xx, yy + 4);
                            }
                        }
                    }
                }
            }
        }
        // Chroma 4:2:0 (vp9_filter_block_plane_ss11). MI is 4 chroma
        // pixels. The UV masks are built per 64x64 superblock exactly as
        // vp9_setup_mask/build_masks do — one build_masks per block, only
        // when its origin sits at an even (MI row, MI col) — then adjusted
        // by vp9_adjust_mask; edges are emitted from the masks. Mask bit i
        // covers row group i>>2 (8 chroma rows) and column group i&3
        // (8 chroma px).
        let sb_rows = (self.mi_rows + 7) / 8;
        let sb_cols = (self.mi_cols + 7) / 8;
        for sbr_i in 0..sb_rows {
            let sbr = sbr_i * 8;
            for sbc_i in 0..sb_cols {
                let sbc = sbc_i * 8;
                let mut left = [0u16; 4];
                let mut above = [0u16; 4];
                let mut int4 = 0u16;
                for r in sbr..(sbr + 8).min(self.mi_rows) {
                    if r % 2 != 0 {
                        continue;
                    }
                    for c in sbc..(sbc + 8).min(self.mi_cols) {
                        if c % 2 != 0 {
                            continue;
                        }
                        let cell = r * self.mi_cols + c;
                        let (bsize, bor, boc) = self.blk[cell];
                        if (bor as usize, boc as usize) != (r, c) || self.level8[cell] == 0 {
                            continue;
                        }
                        let uv_tx = get_uv_tx_size(self.otx[cell] as usize, bsize as usize);
                        let shift = (((r & 7) >> 1) << 2) + ((c & 7) >> 1);
                        let b = bsize as usize;
                        left[uv_tx] |= UV_LEFT_PRED[b] << shift;
                        left[uv_tx] |= (UV_SIZE_MASK[b] & UV_LEFT_TX[uv_tx]) << shift;
                        above[uv_tx] |= UV_ABOVE_PRED[b] << shift;
                        above[uv_tx] |= (UV_SIZE_MASK[b] & UV_ABOVE_TX[uv_tx]) << shift;
                        if uv_tx == TX_4X4 {
                            int4 |= UV_SIZE_MASK[b] << shift;
                        }
                    }
                }
                // vp9_adjust_mask.
                left[TX_16X16] |= left[TX_32X32];
                above[TX_16X16] |= above[TX_32X32];
                left[TX_8X8] |= left[TX_4X4] & 0x1111;
                left[TX_4X4] &= !0x1111;
                above[TX_8X8] |= above[TX_4X4] & 0x000f;
                above[TX_4X4] &= !0x000f;
                if sbr + 8 > self.mi_rows {
                    let rows = self.mi_rows - sbr;
                    let mask_uv = ((1u32 << (((rows + 1) >> 1) << 2)) as u16).wrapping_sub(1);
                    for m in left.iter_mut().take(3) {
                        *m &= mask_uv;
                    }
                    for m in above.iter_mut().take(3) {
                        *m &= mask_uv;
                    }
                    int4 &= mask_uv;
                    if rows == 1 {
                        above[TX_8X8] |= above[TX_16X16];
                        above[TX_16X16] = 0;
                    }
                    if rows == 5 {
                        above[TX_8X8] |= above[TX_16X16] & 0xff00;
                        above[TX_16X16] &= !(above[TX_16X16] & 0xff00);
                    }
                }
                if sbc + 8 > self.mi_cols {
                    let columns = self.mi_cols - sbc;
                    let mask_uv = ((1u32 << ((columns + 1) >> 1)) as u16)
                        .wrapping_sub(1)
                        .wrapping_mul(0x1111);
                    let mask_uv_int = ((1u32 << (columns >> 1)) as u16)
                        .wrapping_sub(1)
                        .wrapping_mul(0x1111);
                    for m in left.iter_mut().take(3) {
                        *m &= mask_uv;
                    }
                    for m in above.iter_mut().take(3) {
                        *m &= mask_uv;
                    }
                    int4 &= mask_uv_int;
                    if columns == 1 {
                        left[TX_8X8] |= left[TX_16X16];
                        left[TX_16X16] = 0;
                    }
                    if columns == 5 {
                        left[TX_8X8] |= left[TX_16X16] & 0xcccc;
                        left[TX_16X16] &= !(left[TX_16X16] & 0xcccc);
                    }
                }
                if sbc == 0 {
                    for m in left.iter_mut().take(3) {
                        *m &= 0xeeee;
                    }
                }
                for data in [&mut *u, &mut *v] {
                    // libvpx runs the whole vertical pass before the
                    // horizontal one; they overlap, so order matters.
                    for rg in 0..4usize {
                        let y8 = sbr * 4 + 8 * rg;
                        if y8 + 8 > uv_h {
                            continue;
                        }
                        for cg in 0..4usize {
                            let x8 = sbc * 4 + 8 * cg;
                            let bit = 1u16 << (rg * 4 + cg);
                            let lvl = self.level8[(sbr + 2 * rg) * self.mi_cols + sbc + 2 * cg];
                            if lvl == 0 {
                                continue;
                            }
                            let (bl, li, th) = limits(lvl, sharpness);
                            if x8 >= 4 && x8 + 4 <= uv_w {
                                let taps = if left[TX_16X16] & bit != 0 {
                                    16
                                } else if left[TX_8X8] & bit != 0 {
                                    8
                                } else if left[TX_4X4] & bit != 0 {
                                    4
                                } else {
                                    0
                                };
                                if taps != 0 {
                                    let t = if taps == 16 && (x8 < 8 || x8 + 8 > uv_w) {
                                        8
                                    } else {
                                        taps
                                    };
                                    lpf_edge(
                                        data,
                                        y8 * uv_stride + x8,
                                        uv_stride,
                                        1,
                                        t,
                                        bl,
                                        li,
                                        th,
                                        8,
                                    );
                                }
                            }
                            if int4 & bit != 0 && x8 + 8 <= uv_w {
                                lpf_edge(
                                    data,
                                    y8 * uv_stride + x8 + 4,
                                    uv_stride,
                                    1,
                                    4,
                                    bl,
                                    li,
                                    th,
                                    8,
                                );
                            }
                        }
                    }
                    for rg in 0..4usize {
                        let y8 = sbr * 4 + 8 * rg;
                        if y8 + 8 > uv_h {
                            continue;
                        }
                        for cg in 0..4usize {
                            let x8 = sbc * 4 + 8 * cg;
                            let bit = 1u16 << (rg * 4 + cg);
                            let lvl = self.level8[(sbr + 2 * rg) * self.mi_cols + sbc + 2 * cg];
                            if lvl == 0 {
                                continue;
                            }
                            let (bl, li, th) = limits(lvl, sharpness);
                            if y8 >= 4 && y8 + 4 <= uv_h && x8 + 8 <= uv_w {
                                let taps = if above[TX_16X16] & bit != 0 {
                                    16
                                } else if above[TX_8X8] & bit != 0 {
                                    8
                                } else if above[TX_4X4] & bit != 0 {
                                    4
                                } else {
                                    0
                                };
                                if taps != 0 {
                                    let t = if taps == 16 && (y8 < 8 || y8 + 8 > uv_h) {
                                        8
                                    } else {
                                        taps
                                    };
                                    lpf_edge(
                                        data,
                                        y8 * uv_stride + x8,
                                        uv_stride,
                                        0,
                                        t,
                                        bl,
                                        li,
                                        th,
                                        8,
                                    );
                                }
                            }
                            if int4 & bit != 0
                                && sbr + 2 * rg != self.mi_rows - 1
                                && y8 + 8 <= uv_h
                                && x8 + 8 <= uv_w
                            {
                                lpf_edge(
                                    data,
                                    (y8 + 4) * uv_stride + x8,
                                    uv_stride,
                                    0,
                                    4,
                                    bl,
                                    li,
                                    th,
                                    8,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

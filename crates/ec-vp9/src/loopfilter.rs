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
    /// Per-4x4 luma transform size, `mi_cols*2 x mi_rows*2`.
    pub tx4: Vec<u8>,
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
        let mc2 = self.mi_cols * 2;
        let mr2 = self.mi_rows * 2;
        if std::env::var_os("LFGRID").is_some() {
            for r in 0..self.mi_rows {
                for c in 0..self.mi_cols {
                    let cell = r * self.mi_cols + c;
                    println!(
                        "G {} {} {} {} {}",
                        r, c, self.blk[cell].0, self.level8[cell], self.otx[cell]
                    );
                }
            }
        }
        // Luma, vertical edges (4x4 column boundaries).
        for r4 in 0..mr2 {
            for c4 in 1..mc2 {
                let cell = (r4 / 2) * self.mi_cols + c4 / 2;
                let level = self.level8[cell];
                if level == 0 {
                    continue;
                }
                // The masks are per-MI: every MI of a block contributes
                // its own left edge (`left_64x64_txform_mask` marks every
                // MI column for TX_8X8, every other for TX_16X16, every
                // fourth for TX_32X32), so internal MI boundaries of wide
                // blocks are filtered too — not just the block origin.
                let mut taps = if c4 % 2 == 0 {
                    let tx = self.otx[cell] as usize;
                    if c4 % 8 == 0 {
                        match tx {
                            TX_16X16 | TX_32X32 => 16,
                            _ => 8,
                        }
                    } else if c4 % 4 == 0 {
                        match tx {
                            TX_32X32 => continue,
                            TX_16X16 => 16,
                            TX_8X8 => 8,
                            _ => 4,
                        }
                    } else {
                        match tx {
                            TX_8X8 => 8,
                            TX_4X4 => 4,
                            _ => continue,
                        }
                    }
                } else if self.tx4[r4 * mc2 + c4] as usize == TX_4X4 {
                    // mask_4x4_int: internal 4x4 edge at the half position.
                    4
                } else {
                    continue;
                };
                // a 16-tap edge needs 8 samples on each side
                if taps == 16 && (c4 * 4 < 8 || c4 * 4 + 8 > y_w) {
                    taps = 8;
                }
                let (bl, li, th) = limits(level, sharpness);
                let off = r4 * 4 * stride + c4 * 4;
                if y_w >= c4 * 4 + 4 {
                    lpf_edge(y, off, stride, 1, taps, bl, li, th, 4);
                    if std::env::var_os("LFTRACE").is_some() {
                        let kind = match taps {
                            16 => "V16",
                            8 => "V8",
                            _ => "V4",
                        };
                        println!("{kind} {} {}", c4 * 4, r4 * 4);
                    }
                }
            }
        }
        // Luma, horizontal edges.
        for r4 in 1..mr2 {
            for c4 in 0..mc2 {
                let cell = (r4 / 2) * self.mi_cols + c4 / 2;
                let level = self.level8[cell];
                if level == 0 {
                    continue;
                }
                // Horizontal mirror of the vertical rule: the above-masks
                // mark every MI row for TX_8X8 (`above_64x64_txform_mask`),
                // every other row for TX_16X16, every fourth for TX_32X32.
                let mut taps = if r4 % 2 == 0 {
                    let tx = self.otx[cell] as usize;
                    if r4 % 8 == 0 {
                        match tx {
                            TX_16X16 | TX_32X32 => 16,
                            _ => 8,
                        }
                    } else if r4 % 4 == 0 {
                        match tx {
                            TX_32X32 => continue,
                            TX_16X16 => 16,
                            TX_8X8 => 8,
                            _ => 4,
                        }
                    } else {
                        match tx {
                            TX_8X8 => 8,
                            TX_4X4 => 4,
                            _ => continue,
                        }
                    }
                } else if self.tx4[r4 * mc2 + c4] as usize == TX_4X4 {
                    // mask_4x4_int: internal 4x4 edge at the half position
                    // (filter_selectively_horiz fires it alongside 8/4-tap
                    // top edges and alone when no top edge exists).
                    4
                } else {
                    continue;
                };
                if taps == 16 && (r4 * 4 < 8 || r4 * 4 + 8 > y_h) {
                    taps = 8;
                }
                let (bl, li, th) = limits(level, sharpness);
                let off = r4 * 4 * stride + c4 * 4;
                if y_h >= r4 * 4 + 4 {
                    lpf_edge(y, off, stride, 0, taps, bl, li, th, 4);
                    if std::env::var_os("LFTRACE").is_some() {
                        let kind = match taps {
                            16 => "H16",
                            8 => "H8",
                            _ => "H4",
                        };
                        println!("{kind} {} {}", c4 * 4, r4 * 4);
                    }
                }
            }
        }
        // Chroma 4:2:0 (vp9_filter_block_plane_ss11): MI is 4 chroma
        // pixels. Edges follow uv tx size, not every MI — TX_8 every 8
        // chroma px (2 MI), TX_16/32 every 16/32. TX_4 hits every MI
        // (int_4x4_uv is those odd columns).
        for data in [&mut *u, &mut *v] {
            for r in 0..self.mi_rows {
                if r % 2 != 0 {
                    continue;
                }
                for c in 1..self.mi_cols {
                    let cell = r * self.mi_cols + c;
                    let (bsize, _bor, _boc) = self.blk[cell];
                    let level = self.level8[cell];
                    if level == 0 {
                        continue;
                    }
                    let uv_tx = uv_txsize_lookup(bsize as usize, self.otx[cell] as usize);
                    let step = 1usize << uv_tx;
                    if c % step != 0 {
                        continue;
                    }
                    // vp9_adjust_mask: 4-tap on a 64x64 left border is
                    // promoted to 8-tap (`left_uv[TX_4] & 0x1111` → TX_8).
                    let mut taps = match uv_tx {
                        TX_16X16 | TX_32X32 if c * 4 >= 8 && c * 4 + 8 <= uv_w => 16,
                        TX_16X16 | TX_32X32 | TX_8X8 => 8,
                        _ => 4,
                    };
                    if taps == 4 && c % 8 == 0 {
                        taps = 8;
                    }
                    let (bl, li, th) = limits(level, sharpness);
                    let off = r * 4 * uv_stride + c * 4;
                    if uv_w >= c * 4 + 4 {
                        lpf_edge(data, off, uv_stride, 1, taps, bl, li, th, 8);
                    }
                }
            }
            for r in 1..self.mi_rows {
                for c in 0..self.mi_cols {
                    if c % 2 != 0 {
                        continue;
                    }
                    let cell = r * self.mi_cols + c;
                    let (bsize, _bor, _boc) = self.blk[cell];
                    let level = self.level8[cell];
                    if level == 0 {
                        continue;
                    }
                    let uv_tx = uv_txsize_lookup(bsize as usize, self.otx[cell] as usize);
                    let step = 1usize << uv_tx;
                    if r % step != 0 {
                        continue;
                    }
                    // above_border_uv 0x000f: 4-tap on a 64x64 top border
                    // is promoted to 8-tap.
                    let mut taps = match uv_tx {
                        TX_16X16 | TX_32X32 if r * 4 >= 8 && r * 4 + 8 <= uv_h => 16,
                        TX_16X16 | TX_32X32 | TX_8X8 => 8,
                        _ => 4,
                    };
                    if taps == 4 && r % 8 == 0 {
                        taps = 8;
                    }
                    let (bl, li, th) = limits(level, sharpness);
                    let off = r * 4 * uv_stride + c * 4;
                    if uv_h >= r * 4 + 4 {
                        lpf_edge(data, off, uv_stride, 0, taps, bl, li, th, 8);
                    }
                }
            }
        }
    }
}

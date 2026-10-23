//! Bit-exact safe-Rust ports of the FFmpeg 8.1 C (non-SIMD) DSP routines needed by
//! the MPEG-4 Part 2 decoder: IDCTs, half-pel / quarter-pel motion compensation,
//! GMC and the (de)quantisers.  All coefficient blocks are in natural row-major
//! order (index = row * 8 + col, identity IDCT permutation).

use core::num::Wrapping as Wr;

#[inline(always)]
fn clip_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

// ---------------------------------------------------------------------------
// simple_idct (ff_simple_idct_put/add_int16_8bit)
// ---------------------------------------------------------------------------

const W1: i32 = 22725;
const W2: i32 = 21407;
const W3: i32 = 19266;
const W4: i32 = 16383;
const W5: i32 = 12873;
const W6: i32 = 8867;
const W7: i32 = 4520;
const ROW_SHIFT: u32 = 11;
const COL_SHIFT: u32 = 20;

#[inline(always)]
fn w(v: i32) -> Wr<i32> {
    Wr(v)
}

/// idctRowCondDC_int16_8bit (extra_shift = 0), in place.
#[inline(always)]
fn simple_row(row: &mut [i16]) {
    let row: &mut [i16; 8] = row.try_into().unwrap();
    if row[1..].iter().all(|&x| x == 0) {
        let t = (row[0] as i32 * 8) as i16;
        *row = [t; 8];
        return;
    }
    let r: [Wr<i32>; 8] = core::array::from_fn(|i| w(row[i] as i32));
    let mut a0 = w(W4) * r[0] + w(1 << (ROW_SHIFT - 1));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 += w(W2) * r[2];
    a1 += w(W6) * r[2];
    a2 -= w(W6) * r[2];
    a3 -= w(W2) * r[2];
    let mut b0 = w(W1) * r[1] + w(W3) * r[3];
    let mut b1 = w(W3) * r[1] - w(W7) * r[3];
    let mut b2 = w(W5) * r[1] - w(W1) * r[3];
    let mut b3 = w(W7) * r[1] - w(W5) * r[3];
    a0 += w(W4) * r[4] + w(W6) * r[6];
    a1 += -(w(W4) * r[4]) - w(W2) * r[6];
    a2 += -(w(W4) * r[4]) + w(W2) * r[6];
    a3 += w(W4) * r[4] - w(W6) * r[6];
    b0 += w(W5) * r[5] + w(W7) * r[7];
    b1 += -(w(W1) * r[5]) - w(W5) * r[7];
    b2 += w(W7) * r[5] + w(W3) * r[7];
    b3 += w(W3) * r[5] - w(W1) * r[7];
    row[0] = ((a0 + b0).0 >> ROW_SHIFT) as i16;
    row[7] = ((a0 - b0).0 >> ROW_SHIFT) as i16;
    row[1] = ((a1 + b1).0 >> ROW_SHIFT) as i16;
    row[6] = ((a1 - b1).0 >> ROW_SHIFT) as i16;
    row[2] = ((a2 + b2).0 >> ROW_SHIFT) as i16;
    row[5] = ((a2 - b2).0 >> ROW_SHIFT) as i16;
    row[3] = ((a3 + b3).0 >> ROW_SHIFT) as i16;
    row[4] = ((a3 - b3).0 >> ROW_SHIFT) as i16;
}

/// IDCT_COLS: returns the 8 output values (already shifted) for column `c`.
#[inline(always)]
fn simple_col(blk: &[i16; 64], c: usize) -> [i32; 8] {
    let col: [Wr<i32>; 8] = core::array::from_fn(|i| w(blk[i * 8 + c] as i32));
    let mut a0 = w(W4) * (col[0] + w((1 << (COL_SHIFT - 1)) / W4));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 += w(W2) * col[2];
    a1 += w(W6) * col[2];
    a2 += w(-W6) * col[2];
    a3 += w(-W2) * col[2];
    let mut b0 = w(W1) * col[1];
    let mut b1 = w(W3) * col[1];
    let mut b2 = w(W5) * col[1];
    let mut b3 = w(W7) * col[1];
    b0 += w(W3) * col[3];
    b1 += w(-W7) * col[3];
    b2 += w(-W1) * col[3];
    b3 += w(-W5) * col[3];
    a0 += w(W4) * col[4];
    a1 += w(-W4) * col[4];
    a2 += w(-W4) * col[4];
    a3 += w(W4) * col[4];
    b0 += w(W5) * col[5];
    b1 += w(-W1) * col[5];
    b2 += w(W7) * col[5];
    b3 += w(W3) * col[5];
    a0 += w(W6) * col[6];
    a1 += w(-W2) * col[6];
    a2 += w(W2) * col[6];
    a3 += w(-W6) * col[6];
    b0 += w(W7) * col[7];
    b1 += w(-W5) * col[7];
    b2 += w(W3) * col[7];
    b3 += w(-W1) * col[7];
    [
        (a0 + b0).0 >> COL_SHIFT,
        (a1 + b1).0 >> COL_SHIFT,
        (a2 + b2).0 >> COL_SHIFT,
        (a3 + b3).0 >> COL_SHIFT,
        (a3 - b3).0 >> COL_SHIFT,
        (a2 - b2).0 >> COL_SHIFT,
        (a1 - b1).0 >> COL_SHIFT,
        (a0 - b0).0 >> COL_SHIFT,
    ]
}

#[inline(always)]
fn check_dst(dst: &[u8], stride: usize) {
    assert!(dst.len() >= 7 * stride + 8, "dst too small for 8x8 block");
}

/// ff_simple_idct_put_int16_8bit. `block` is clobbered (row pass is done in place;
/// the caller normally zeroes it afterwards). `dst` starts at the block's top-left pixel.
pub fn idct_simple_put(block: &mut [i16; 64], dst: &mut [u8], dst_stride: usize) {
    check_dst(dst, dst_stride);
    for r in block.as_chunks_mut::<8>().0 {
        simple_row(r);
    }
    for c in 0..8 {
        let o = simple_col(block, c);
        for r in 0..8 {
            dst[r * dst_stride + c] = clip_u8(o[r]);
        }
    }
}

/// ff_simple_idct_add_int16_8bit. Same conventions as [`idct_simple_put`].
pub fn idct_simple_add(block: &mut [i16; 64], dst: &mut [u8], dst_stride: usize) {
    check_dst(dst, dst_stride);
    for r in block.as_chunks_mut::<8>().0 {
        simple_row(r);
    }
    for c in 0..8 {
        let o = simple_col(block, c);
        for r in 0..8 {
            let p = &mut dst[r * dst_stride + c];
            *p = clip_u8(*p as i32 + o[r]);
        }
    }
}

// ---------------------------------------------------------------------------
// xvid idct (ff_xvid_idct, FF_IDCT_PERM_NONE => natural order)
// ---------------------------------------------------------------------------

const XV_ROW_SHIFT: u32 = 11;
const XV_COL_SHIFT: u32 = 6;
const RND0: i32 = 65536;
const RND1: i32 = 3597;
const RND2: i32 = 2260;
const RND3: i32 = 1203;
const RND4: i32 = 0;
const RND5: i32 = 120;
const RND6: i32 = 512;
const RND7: i32 = 512;

const TAB04: [i32; 7] = [22725, 21407, 19266, 16384, 12873, 8867, 4520];
const TAB17: [i32; 7] = [31521, 29692, 26722, 22725, 17855, 12299, 6270];
const TAB26: [i32; 7] = [29692, 27969, 25172, 21407, 16819, 11585, 5906];
const TAB35: [i32; 7] = [26722, 25172, 22654, 19266, 15137, 10426, 5315];

/// idct_row of xvididct.c; returns false iff the row was left untouched (all-zero result).
#[inline(always)]
fn xv_row(row: &mut [i16], tab: &[i32; 7], rnd: i32) -> bool {
    let row: &mut [i16; 8] = row.try_into().unwrap();
    let c: [Wr<i32>; 7] = core::array::from_fn(|i| w(tab[i]));
    let (c1, c2, c3, c4, c5, c6, c7) = (c[0], c[1], c[2], c[3], c[4], c[5], c[6]);
    let i: [Wr<i32>; 8] = core::array::from_fn(|k| w(row[k] as i32));
    let right = row[5] | row[6] | row[7];
    let left = row[1] | row[2] | row[3];
    let rnd = w(rnd);
    let sh = XV_ROW_SHIFT;
    if (right | row[4]) == 0 {
        let k = c4 * i[0] + rnd;
        if left != 0 {
            let a0 = k + c2 * i[2];
            let a1 = k + c6 * i[2];
            let a2 = k - c6 * i[2];
            let a3 = k - c2 * i[2];
            let b0 = c1 * i[1] + c3 * i[3];
            let b1 = c3 * i[1] - c7 * i[3];
            let b2 = c5 * i[1] - c1 * i[3];
            let b3 = c7 * i[1] - c5 * i[3];
            row[0] = ((a0 + b0).0 >> sh) as i16;
            row[1] = ((a1 + b1).0 >> sh) as i16;
            row[2] = ((a2 + b2).0 >> sh) as i16;
            row[3] = ((a3 + b3).0 >> sh) as i16;
            row[4] = ((a3 - b3).0 >> sh) as i16;
            row[5] = ((a2 - b2).0 >> sh) as i16;
            row[6] = ((a1 - b1).0 >> sh) as i16;
            row[7] = ((a0 - b0).0 >> sh) as i16;
        } else {
            let a0 = k.0 >> sh;
            if a0 != 0 {
                *row = [a0 as i16; 8];
            } else {
                return false;
            }
        }
    } else if (left | right) == 0 {
        let a0 = ((rnd + c4 * (i[0] + i[4])).0 >> sh) as i16;
        let a1 = ((rnd + c4 * (i[0] - i[4])).0 >> sh) as i16;
        row[0] = a0;
        row[3] = a0;
        row[4] = a0;
        row[7] = a0;
        row[1] = a1;
        row[2] = a1;
        row[5] = a1;
        row[6] = a1;
    } else {
        let k = c4 * i[0] + rnd;
        let a0 = k + c2 * i[2] + c4 * i[4] + c6 * i[6];
        let a1 = k + c6 * i[2] - c4 * i[4] - c2 * i[6];
        let a2 = k - c6 * i[2] - c4 * i[4] + c2 * i[6];
        let a3 = k - c2 * i[2] + c4 * i[4] - c6 * i[6];
        let b0 = c1 * i[1] + c3 * i[3] + c5 * i[5] + c7 * i[7];
        let b1 = c3 * i[1] - c7 * i[3] - c1 * i[5] - c5 * i[7];
        let b2 = c5 * i[1] - c1 * i[3] + c7 * i[5] + c3 * i[7];
        let b3 = c7 * i[1] - c5 * i[3] + c3 * i[5] - c1 * i[7];
        row[0] = ((a0 + b0).0 >> sh) as i16;
        row[1] = ((a1 + b1).0 >> sh) as i16;
        row[2] = ((a2 + b2).0 >> sh) as i16;
        row[3] = ((a3 + b3).0 >> sh) as i16;
        row[4] = ((a3 - b3).0 >> sh) as i16;
        row[5] = ((a2 - b2).0 >> sh) as i16;
        row[6] = ((a1 - b1).0 >> sh) as i16;
        row[7] = ((a0 - b0).0 >> sh) as i16;
    }
    true
}

const TAN1: i32 = 0x32EC;
const TAN2: i32 = 0x6A0A;
const TAN3: i32 = 0xAB0E;
const SQRT2: i32 = 0x5A82;

#[inline(always)]
fn mult(c: i32, x: i32) -> i32 {
    c.wrapping_mul(x) >> 16
}

#[inline(always)]
fn bf(a: i32, b: i32) -> (i32, i32) {
    (a.wrapping_add(b), a.wrapping_sub(b))
}

/// Shared tail of idct_col_8/4/3. Returns the 8 output samples (shifted, as i16 later).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn xv_tail(
    mm0: i32,
    mm1: i32,
    mm2: i32,
    mm3: i32,
    mm4: i32,
    mm5: i32,
    mm6: i32,
    mm7: i32,
) -> [i16; 8] {
    let s = XV_COL_SHIFT;
    let (mm0, mm3) = bf(mm0, mm3);
    let (mm0, mm7) = bf(mm0, mm7);
    let (mm3, mm4) = bf(mm3, mm4);
    let (mm1, mm2) = bf(mm1, mm2);
    let (mm1, mm6) = bf(mm1, mm6);
    let (mm2, mm5) = bf(mm2, mm5);
    [
        (mm0 >> s) as i16,
        (mm1 >> s) as i16,
        (mm2 >> s) as i16,
        (mm3 >> s) as i16,
        (mm4 >> s) as i16,
        (mm5 >> s) as i16,
        (mm6 >> s) as i16,
        (mm7 >> s) as i16,
    ]
}

#[inline(always)]
fn xv_col8(c: [i32; 8]) -> [i16; 8] {
    let (mm4, mm5, mm6, mm7) = (c[7], c[5], c[3], c[1]);
    let mm0 = mult(TAN1, mm4).wrapping_add(mm7);
    let mm1 = mult(TAN1, mm7).wrapping_sub(mm4);
    let mm2 = mult(TAN3, mm5).wrapping_add(mm6);
    let mm3 = mult(TAN3, mm6).wrapping_sub(mm5);
    let mm7 = mm0.wrapping_add(mm2);
    let mm4 = mm1.wrapping_sub(mm3);
    let mm0 = mm0.wrapping_sub(mm2);
    let mm1 = mm1.wrapping_add(mm3);
    let mm6 = mm0.wrapping_add(mm1);
    let mm5 = mm0.wrapping_sub(mm1);
    let mm5 = mult(SQRT2, mm5).wrapping_mul(2);
    let mm6 = mult(SQRT2, mm6).wrapping_mul(2);
    let mm1 = c[2];
    let mm2 = c[6];
    let mm3 = mult(TAN2, mm2).wrapping_add(mm1);
    let mm2 = mult(TAN2, mm1).wrapping_sub(mm2);
    let (mm0, mm1) = bf(c[0], c[4]);
    xv_tail(mm0, mm1, mm2, mm3, mm4, mm5, mm6, mm7)
}

#[inline(always)]
fn xv_col4(c: [i32; 8]) -> [i16; 8] {
    let mm0 = c[1];
    let mm2 = c[3];
    let mm1 = mult(TAN1, mm0);
    let mm3 = mult(TAN3, mm2);
    let mm7 = mm0.wrapping_add(mm2);
    let mm4 = mm1.wrapping_sub(mm3);
    let mm0 = mm0.wrapping_sub(mm2);
    let mm1 = mm1.wrapping_add(mm3);
    let mm6 = mm0.wrapping_add(mm1);
    let mm5 = mm0.wrapping_sub(mm1);
    let mm6 = mult(SQRT2, mm6).wrapping_mul(2);
    let mm5 = mult(SQRT2, mm5).wrapping_mul(2);
    let mm0 = c[0];
    let mm1 = c[0];
    let mm3 = c[2];
    let mm2 = mult(TAN2, mm3);
    xv_tail(mm0, mm1, mm2, mm3, mm4, mm5, mm6, mm7)
}

#[inline(always)]
fn xv_col3(c: [i32; 8]) -> [i16; 8] {
    let mm7 = c[1];
    let mm4 = mult(TAN1, mm7);
    let mm6 = mm7.wrapping_add(mm4);
    let mm5 = mm7.wrapping_sub(mm4);
    let mm6 = mult(SQRT2, mm6).wrapping_mul(2);
    let mm5 = mult(SQRT2, mm5).wrapping_mul(2);
    let mm0 = c[0];
    let mm1 = c[0];
    let mm3 = c[2];
    let mm2 = mult(TAN2, mm3);
    xv_tail(mm0, mm1, mm2, mm3, mm4, mm5, mm6, mm7)
}

/// ff_xvid_idct in place (natural order in and out).
fn xvid_idct(b: &mut [i16; 64]) {
    let mut rows = 0x07u32;
    xv_row(&mut b[0..8], &TAB04, RND0);
    xv_row(&mut b[8..16], &TAB17, RND1);
    xv_row(&mut b[16..24], &TAB26, RND2);
    if xv_row(&mut b[24..32], &TAB35, RND3) {
        rows |= 0x08;
    }
    if xv_row(&mut b[32..40], &TAB04, RND4) {
        rows |= 0x10;
    }
    if xv_row(&mut b[40..48], &TAB35, RND5) {
        rows |= 0x20;
    }
    if xv_row(&mut b[48..56], &TAB26, RND6) {
        rows |= 0x40;
    }
    if xv_row(&mut b[56..64], &TAB17, RND7) {
        rows |= 0x80;
    }
    for c in 0..8 {
        let col: [i32; 8] = core::array::from_fn(|r| b[r * 8 + c] as i32);
        let o = if rows & 0xF0 != 0 {
            xv_col8(col)
        } else if rows & 0x08 != 0 {
            xv_col4(col)
        } else {
            xv_col3(col)
        };
        for r in 0..8 {
            b[r * 8 + c] = o[r];
        }
    }
}

/// ff_xvid_idct_put (idct + put_pixels_clamped). `block` is overwritten with the IDCT output.
pub fn idct_xvid_put(block: &mut [i16; 64], dst: &mut [u8], dst_stride: usize) {
    check_dst(dst, dst_stride);
    xvid_idct(block);
    for r in 0..8 {
        let d = &mut dst[r * dst_stride..][..8];
        for (p, &v) in d.iter_mut().zip(&block[r * 8..r * 8 + 8]) {
            *p = clip_u8(v as i32);
        }
    }
}

/// ff_xvid_idct_add (idct + add_pixels_clamped). `block` is overwritten with the IDCT output.
pub fn idct_xvid_add(block: &mut [i16; 64], dst: &mut [u8], dst_stride: usize) {
    check_dst(dst, dst_stride);
    xvid_idct(block);
    for r in 0..8 {
        let d = &mut dst[r * dst_stride..][..8];
        for (p, &v) in d.iter_mut().zip(&block[r * 8..r * 8 + 8]) {
            *p = clip_u8(*p as i32 + v as i32);
        }
    }
}

// ---------------------------------------------------------------------------
// Half-pel (hpeldsp.c)
// ---------------------------------------------------------------------------

#[inline(always)]
fn hpel_run<const W: usize, const AVG: bool, const MODE: u8, const NR: bool>(
    dst: &mut [u8],
    ds: usize,
    src: &[u8],
    ss: usize,
    h: usize,
) {
    for r in 0..h {
        let d: &mut [u8; W] = (&mut dst[r * ds..][..W]).try_into().unwrap();
        let v: [u8; W] = match MODE {
            0 => (&src[r * ss..][..W]).try_into().unwrap(),
            1 => {
                let a: &[u8; W] = (&src[r * ss..][..W]).try_into().unwrap();
                let b: &[u8; W] = (&src[r * ss + 1..][..W]).try_into().unwrap();
                core::array::from_fn(|i| ((a[i] as u16 + b[i] as u16 + (!NR) as u16) >> 1) as u8)
            }
            2 => {
                let a: &[u8; W] = (&src[r * ss..][..W]).try_into().unwrap();
                let b: &[u8; W] = (&src[(r + 1) * ss..][..W]).try_into().unwrap();
                core::array::from_fn(|i| ((a[i] as u16 + b[i] as u16 + (!NR) as u16) >> 1) as u8)
            }
            _ => {
                let a: &[u8] = &src[r * ss..][..W + 1];
                let b: &[u8] = &src[(r + 1) * ss..][..W + 1];
                let rc = if NR { 1u16 } else { 2u16 };
                core::array::from_fn(|i| {
                    ((a[i] as u16 + a[i + 1] as u16 + b[i] as u16 + b[i + 1] as u16 + rc) >> 2)
                        as u8
                })
            }
        };
        if AVG {
            for (p, &q) in d.iter_mut().zip(v.iter()) {
                *p = ((*p as u16 + q as u16 + 1) >> 1) as u8;
            }
        } else {
            *d = v;
        }
    }
}

#[inline(always)]
fn hpel_mode<const W: usize, const AVG: bool>(
    dst: &mut [u8],
    ds: usize,
    src: &[u8],
    ss: usize,
    h: usize,
    mode: usize,
    nr: bool,
) {
    match (mode & 3, nr) {
        (0, _) => hpel_run::<W, AVG, 0, false>(dst, ds, src, ss, h),
        (1, false) => hpel_run::<W, AVG, 1, false>(dst, ds, src, ss, h),
        (1, true) => hpel_run::<W, AVG, 1, true>(dst, ds, src, ss, h),
        (2, false) => hpel_run::<W, AVG, 2, false>(dst, ds, src, ss, h),
        (2, true) => hpel_run::<W, AVG, 2, true>(dst, ds, src, ss, h),
        (_, false) => hpel_run::<W, AVG, 3, false>(dst, ds, src, ss, h),
        (_, true) => hpel_run::<W, AVG, 3, true>(dst, ds, src, ss, h),
    }
}

/// Half-pel put: put_pixels{16,8,4,2}_{,x2,y2,xy2}_8_c / put_no_rnd_pixels{16,8}_*.
/// `dxy` bit0 = half-pel in x, bit1 = half-pel in y.
///
/// Source samples read: rows `0..h` (+1 if dxy&2), columns `0..w` (+1 if dxy&1); xy2 reads
/// the full (w+1)x(h+1). ffmpeg only has `put_no_rnd` tables for w = 16 and 8, so
/// `no_rnd` is ignored (rounding variant used) for w = 4 and 2.
/// Rounding: x2/y2 = (a+b+1)>>1 (no_rnd: (a+b)>>1); xy2 = (a+b+c+d+2)>>2 (no_rnd: +1).
pub fn put_hpel(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    w: usize,
    h: usize,
    dxy: usize,
    no_rnd: bool,
) {
    if h == 0 {
        return;
    }
    match w {
        16 => hpel_mode::<16, false>(dst, dst_stride, src, src_stride, h, dxy, no_rnd),
        8 => hpel_mode::<8, false>(dst, dst_stride, src, src_stride, h, dxy, no_rnd),
        4 => hpel_mode::<4, false>(dst, dst_stride, src, src_stride, h, dxy, false),
        2 => hpel_mode::<2, false>(dst, dst_stride, src, src_stride, h, dxy, false),
        _ => panic!("put_hpel: unsupported width {w}"),
    }
}

/// Half-pel avg: avg_pixels{16,8,4,2}_* / avg_no_rnd_pixels16_*. Result is
/// `(dst + interp + 1) >> 1` (rnd_avg32).
///
/// ffmpeg has `avg_no_rnd_pixels_tab` only for w = 16 (the interpolation itself uses the
/// no-rounding form, the final average with dst still rounds up; avg_no_rnd_pixels16 (dxy=0)
/// is avg_pixels16). For w = 8/4/2 `no_rnd` is ignored.
/// Quirk reproduced from the C: `avg_pixels2_xy2_8_c` does NOT average with dst (it just
/// stores, like put) -- the hand-written 2-wide xy2 loop has `/* FIXME non put */`.
pub fn avg_hpel(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    w: usize,
    h: usize,
    dxy: usize,
    no_rnd: bool,
) {
    if h == 0 {
        return;
    }
    match w {
        16 => hpel_mode::<16, true>(dst, dst_stride, src, src_stride, h, dxy, no_rnd),
        8 => hpel_mode::<8, true>(dst, dst_stride, src, src_stride, h, dxy, false),
        4 => hpel_mode::<4, true>(dst, dst_stride, src, src_stride, h, dxy, false),
        2 => {
            if dxy & 3 == 3 {
                hpel_mode::<2, false>(dst, dst_stride, src, src_stride, h, dxy, false)
            } else {
                hpel_mode::<2, true>(dst, dst_stride, src, src_stride, h, dxy, false)
            }
        }
        _ => panic!("avg_hpel: unsupported width {w}"),
    }
}

// ---------------------------------------------------------------------------
// MPEG-4 quarter-pel (qpeldsp.c)
// ---------------------------------------------------------------------------

/// Horizontal MPEG-4 qpel lowpass of one row. `row` must hold S+1 samples.
/// Window edges are mirrored exactly like mpeg4_qpel{8,16}_h_lowpass.
#[inline(always)]
fn qp_h_row<const S: usize, const NR: bool>(row: &[u8]) -> [u8; S] {
    let row = &row[..S + 1];
    // p[k] = row[reflect(k - 3)], reflect(j<0) = -1-j, reflect(j>S) = 2S+1-j
    let mut p = [0i16; 32];
    for (k, q) in p[..S + 7].iter_mut().enumerate() {
        let j = k as isize - 3;
        let j = if j < 0 {
            -1 - j
        } else if j > S as isize {
            2 * S as isize + 1 - j
        } else {
            j
        };
        *q = row[j as usize] as i16;
    }
    let rc: i16 = if NR { 15 } else { 16 };
    core::array::from_fn(|i| {
        let v = (p[i + 3] + p[i + 4]) * 20 - (p[i + 2] + p[i + 5]) * 6 + (p[i + 1] + p[i + 6]) * 3
            - (p[i] + p[i + 7]);
        ((v + rc) >> 5).clamp(0, 255) as u8
    })
}

/// Vertical lowpass over a buffer with row stride `bs` holding S+1 rows (cols 0..S).
/// Output is S rows of S, row stride S (indexed r*S + c).
#[inline(always)]
fn qp_v<const S: usize, const NR: bool>(buf: &[u8], bs: usize, out: &mut [u8; 256]) {
    let rc: i16 = if NR { 15 } else { 16 };
    let rf = |j: isize| -> usize {
        (if j < 0 {
            -1 - j
        } else if j > S as isize {
            2 * S as isize + 1 - j
        } else {
            j
        }) as usize
    };
    for r in 0..S {
        let ri = r as isize;
        let row = |d: isize| -> &[u8; S] { (&buf[rf(ri + d) * bs..][..S]).try_into().unwrap() };
        let (m3, m2, m1, p0, p1, p2, p3, p4) = (
            row(-3),
            row(-2),
            row(-1),
            row(0),
            row(1),
            row(2),
            row(3),
            row(4),
        );
        let o: &mut [u8; S] = (&mut out[r * S..][..S]).try_into().unwrap();
        for c in 0..S {
            let v = (p0[c] as i16 + p1[c] as i16) * 20 - (m1[c] as i16 + p2[c] as i16) * 6
                + (m2[c] as i16 + p3[c] as i16) * 3
                - (m3[c] as i16 + p4[c] as i16);
            o[c] = ((v + rc) >> 5).clamp(0, 255) as u8;
        }
    }
}

#[inline(always)]
fn l2b<const NR: bool>(a: u8, b: u8) -> u8 {
    ((a as u16 + b as u16 + (!NR) as u16) >> 1) as u8
}

#[inline(always)]
fn qp_store<const S: usize>(dst: &mut [u8], ds: usize, res: &[u8; 256], avg: bool) {
    for r in 0..S {
        let d: &mut [u8; S] = (&mut dst[r * ds..][..S]).try_into().unwrap();
        let s: &[u8; S] = (&res[r * S..][..S]).try_into().unwrap();
        if avg {
            for (p, &q) in d.iter_mut().zip(s.iter()) {
                *p = ((*p as u16 + q as u16 + 1) >> 1) as u8;
            }
        } else {
            *d = *s;
        }
    }
}

#[inline(always)]
fn qpel_impl<const S: usize, const NR: bool>(
    dst: &mut [u8],
    ds: usize,
    src: &[u8],
    ss: usize,
    mc: usize,
    avg: bool,
) {
    let dx = mc & 3;
    let dy = mc >> 2;
    let mut res = [0u8; 256];
    let fs = S + 1; // stride of the `full` window copy
    // window copy: rows 0..=S, cols 0..=S (cols 0..S only when no horizontal part)
    let mut full = [0u8; 17 * 17];
    let copy_full = |full: &mut [u8; 17 * 17], cols: usize| {
        for r in 0..=S {
            full[r * fs..r * fs + cols].copy_from_slice(&src[r * ss..][..cols]);
        }
    };
    match (dx, dy) {
        (0, 0) => {
            for r in 0..S {
                let s = &src[r * ss..][..S];
                res[r * S..r * S + S].copy_from_slice(s);
            }
        }
        (1..=3, 0) => {
            // mc10, mc20, mc30: rows 0..S-1, cols 0..=S
            for r in 0..S {
                let h = qp_h_row::<S, NR>(&src[r * ss..][..S + 1]);
                let o = &mut res[r * S..r * S + S];
                match dx {
                    2 => o.copy_from_slice(&h),
                    1 => {
                        let s = &src[r * ss..][..S];
                        for i in 0..S {
                            o[i] = l2b::<NR>(s[i], h[i]);
                        }
                    }
                    _ => {
                        let s = &src[r * ss + 1..][..S];
                        for i in 0..S {
                            o[i] = l2b::<NR>(s[i], h[i]);
                        }
                    }
                }
            }
        }
        (0, 1..=3) => {
            // mc01, mc02, mc03: rows 0..=S, cols 0..S-1
            copy_full(&mut full, S);
            let mut v = [0u8; 256];
            qp_v::<S, NR>(&full, fs, &mut v);
            match dy {
                2 => res = v,
                _ => {
                    let off = if dy == 1 { 0 } else { fs };
                    for r in 0..S {
                        for c in 0..S {
                            res[r * S + c] = l2b::<NR>(full[off + r * fs + c], v[r * S + c]);
                        }
                    }
                }
            }
        }
        _ => {
            // both fractional: needs the (S+1)x(S+1) window
            // halfH: S+1 rows of S, stride S
            let mut half_h = [0u8; 17 * 16];
            // Which `full` offset is averaged into halfH: mc1y -> full, mc3y -> full+1.
            // For dx == 2 (mc21, mc22, mc23) halfH is computed straight from src.
            if dx == 2 {
                for r in 0..=S {
                    let h = qp_h_row::<S, NR>(&src[r * ss..][..S + 1]);
                    half_h[r * S..r * S + S].copy_from_slice(&h);
                }
            } else {
                copy_full(&mut full, S + 1);
                let fo = if dx == 1 { 0 } else { 1 };
                for r in 0..=S {
                    let h = qp_h_row::<S, NR>(&full[r * fs..][..S + 1]);
                    for c in 0..S {
                        half_h[r * S + c] = l2b::<NR>(h[c], full[r * fs + fo + c]);
                    }
                }
            }
            if dy == 2 {
                // mc12, mc22, mc32: V lowpass of halfH (final op)
                qp_v::<S, NR>(&half_h, S, &mut res);
            } else {
                let mut hv = [0u8; 256];
                qp_v::<S, NR>(&half_h, S, &mut hv);
                let off = if dy == 1 { 0 } else { S };
                for r in 0..S {
                    for c in 0..S {
                        res[r * S + c] = l2b::<NR>(half_h[off + r * S + c], hv[r * S + c]);
                    }
                }
            }
        }
    }
    qp_store::<S>(dst, ds, &res, avg);
}

fn check_qpel(dst: &[u8], ds: usize, src: &[u8], ss: usize, size: usize, mc: usize) {
    assert!((size == 8 || size == 16) && mc < 16);
    assert!(dst.len() >= (size - 1) * ds + size);
    let (dx, dy) = (mc & 3, mc >> 2);
    let rows = size - 1 + (dy != 0) as usize;
    let cols = size + (dx != 0) as usize;
    // both fractional / vertical-only copies read the full (S+1) rows, see doc comments
    let rows = if dx != 0 && dy != 0 { size } else { rows };
    assert!(src.len() >= (rows) * ss + cols, "src window too small");
}

/// Quarter-pel MPEG-4 put: put_qpel{8,16}_mcXY_c (mc = X + 4*Y, X = horizontal quarter
/// position 0..3, Y = vertical) and put_no_rnd_qpel*_mcXY_c when `no_rnd` (rounding
/// constant 15 in the lowpass, `(a+b)>>1` in the l2 averages).
///
/// Source samples read (relative to `src[0]` = block top-left, `src_stride` rows):
/// * mc00: rows 0..size-1, cols 0..size-1
/// * X!=0, Y==0: rows 0..size-1, cols 0..=size
/// * X==0, Y!=0: rows 0..=size, cols 0..size-1
/// * both nonzero (incl. mc21/22/23 which only read cols 0..=size too): rows 0..=size, cols 0..=size
///
/// i.e. never more than the (size+1)x(size+1) window starting at src[0]; the lowpass filters
/// mirror at that window's edges. `size` is 8 or 16.
pub fn put_qpel(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    size: usize,
    mc: usize,
    no_rnd: bool,
) {
    check_qpel(dst, dst_stride, src, src_stride, size, mc);
    match (size, no_rnd) {
        (8, false) => qpel_impl::<8, false>(dst, dst_stride, src, src_stride, mc, false),
        (8, true) => qpel_impl::<8, true>(dst, dst_stride, src, src_stride, mc, false),
        (16, false) => qpel_impl::<16, false>(dst, dst_stride, src, src_stride, mc, false),
        _ => qpel_impl::<16, true>(dst, dst_stride, src, src_stride, mc, false),
    }
}

/// Quarter-pel MPEG-4 avg: avg_qpel{8,16}_mcXY_c: `dst = (dst + put_result + 1) >> 1`.
/// ffmpeg has no avg_no_rnd_qpel functions, so `no_rnd` is accepted for symmetry but ignored.
/// Same source window as [`put_qpel`].
pub fn avg_qpel(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    size: usize,
    mc: usize,
    _no_rnd: bool,
) {
    check_qpel(dst, dst_stride, src, src_stride, size, mc);
    match size {
        8 => qpel_impl::<8, false>(dst, dst_stride, src, src_stride, mc, true),
        _ => qpel_impl::<16, false>(dst, dst_stride, src, src_stride, mc, true),
    }
}

// ---------------------------------------------------------------------------
// GMC (mpeg4videodsp.c)
// ---------------------------------------------------------------------------

/// gmc1_c: 8-wide, `h` rows. Reads 9 columns x (h+1) rows of `src` starting at src[0].
/// dst = (A*s[0] + B*s[1] + C*s[stride] + D*s[stride+1] + rounder) >> 8 (truncated to u8).
pub fn gmc1(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    h: usize,
    x16: i32,
    y16: i32,
    rounder: i32,
) {
    let a = (16 - x16) * (16 - y16);
    let b = x16 * (16 - y16);
    let c = (16 - x16) * y16;
    let d = x16 * y16;
    for i in 0..h {
        let s0: &[u8; 9] = (&src[i * src_stride..][..9]).try_into().unwrap();
        let s1: &[u8; 9] = (&src[(i + 1) * src_stride..][..9]).try_into().unwrap();
        let o: &mut [u8; 8] = (&mut dst[i * dst_stride..][..8]).try_into().unwrap();
        for x in 0..8 {
            o[x] = ((a * s0[x] as i32
                + b * s0[x + 1] as i32
                + c * s1[x] as i32
                + d * s1[x + 1] as i32
                + rounder)
                >> 8) as u8;
        }
    }
}

/// ff_gmc_c: 8-wide, `h` rows. `src` is the whole plane (index 0 = pixel (0,0), row pitch
/// `src_stride`), `width`/`height` the plane dimensions used for edge clamping. Note the C
/// uses a single `stride` for both buffers; here they are separate.
#[allow(clippy::too_many_arguments)]
pub fn gmc(
    dst: &mut [u8],
    dst_stride: usize,
    src: &[u8],
    src_stride: usize,
    h: usize,
    mut ox: i32,
    mut oy: i32,
    dxx: i32,
    dxy: i32,
    dyx: i32,
    dyy: i32,
    shift: i32,
    r: i32,
    width: i32,
    height: i32,
) {
    let s = 1i32 << shift;
    let width = width - 1;
    let height = height - 1;
    let st = src_stride as i32;
    for y in 0..h {
        let mut vx = ox;
        let mut vy = oy;
        for x in 0..8 {
            let mut src_x = vx >> 16;
            let mut src_y = vy >> 16;
            let frac_x = src_x & (s - 1);
            let frac_y = src_y & (s - 1);
            src_x >>= shift;
            src_y >>= shift;
            let px = |i: i32| src[i as usize] as i32;
            let v: i32 = if (src_x as u32) < (width as u32) {
                if (src_y as u32) < (height as u32) {
                    let index = src_x + src_y * st;
                    ((px(index) * (s - frac_x) + px(index + 1) * frac_x) * (s - frac_y)
                        + (px(index + st) * (s - frac_x) + px(index + st + 1) * frac_x) * frac_y
                        + r)
                        >> (shift * 2)
                } else {
                    let index = src_x + src_y.clamp(0, height) * st;
                    ((px(index) * (s - frac_x) + px(index + 1) * frac_x) * s + r) >> (shift * 2)
                }
            } else if (src_y as u32) < (height as u32) {
                let index = src_x.clamp(0, width) + src_y * st;
                ((px(index) * (s - frac_y) + px(index + st) * frac_y) * s + r) >> (shift * 2)
            } else {
                let index = src_x.clamp(0, width) + src_y.clamp(0, height) * st;
                px(index)
            };
            dst[y * dst_stride + x] = v as u8;
            vx = vx.wrapping_add(dxx);
            vy = vy.wrapping_add(dyx);
        }
        ox = ox.wrapping_add(dxy);
        oy = oy.wrapping_add(dyy);
    }
}

// ---------------------------------------------------------------------------
// Dequantisation (mpegvideo_unquantize.c)
// ---------------------------------------------------------------------------

/// dct_unquantize_h263_intra_c.
///
/// * `dc_scale`: y_dc_scale (blocks 0..3) or c_dc_scale (blocks 4,5). `block[0] *= dc_scale`
///   is done HERE when `!aic` (so the caller must put the raw/predicted DC *level* in
///   block[0]); with `aic` block[0] is left untouched.
/// * `ac_pred`: if true all of indices 1..=63 are processed, otherwise
///   indices `1..=raster_end` where `raster_end = intra_scantable.raster_end[last_index]`
///   (max natural-order index among scan positions 0..=last_index; with identity permutation
///   this is the running max of `scan[0..=last_index]`).
/// * qmul = 2*qscale, qadd = (qscale-1)|1 (0 with aic). Stores truncate to i16.
///   ffmpeg calls this for every intra block of an MPEG-4 stream without mpeg_quant, even when
///   block_last_index < 0 (then raster_end = -1 -> pass `raster_end = 0`).
pub fn unquant_h263_intra(
    block: &mut [i16; 64],
    qscale: i32,
    dc_scale: i32,
    aic: bool,
    ac_pred: bool,
    raster_end: usize,
) {
    let qmul = qscale << 1;
    let qadd = if !aic {
        block[0] = (block[0] as i32).wrapping_mul(dc_scale) as i16;
        (qscale - 1) | 1
    } else {
        0
    };
    let n = if ac_pred { 63 } else { raster_end.min(63) };
    for b in &mut block[1..=n] {
        let level = *b as i32;
        if level != 0 {
            *b = if level < 0 {
                level.wrapping_mul(qmul).wrapping_sub(qadd)
            } else {
                level.wrapping_mul(qmul).wrapping_add(qadd)
            } as i16;
        }
    }
}

/// dct_unquantize_h263_inter_c: processes natural indices `0..=raster_end`
/// (`raster_end = inter_scantable.raster_end[last_index]`, last_index >= 0).
/// NOTE: the MPEG-4 decoder in ffmpeg never calls this (H.263-quant inter coefficients
/// are dequantised during VLC parsing with qmul/qadd); provided for completeness.
pub fn unquant_h263_inter(block: &mut [i16; 64], qscale: i32, raster_end: usize) {
    let qadd = (qscale - 1) | 1;
    let qmul = qscale << 1;
    for b in &mut block[..=raster_end.min(63)] {
        let level = *b as i32;
        if level != 0 {
            *b = if level < 0 {
                level.wrapping_mul(qmul).wrapping_sub(qadd)
            } else {
                level.wrapping_mul(qmul).wrapping_add(qadd)
            } as i16;
        }
    }
}

/// dct_unquantize_mpeg2_intra_c / _bitexact (MPEG-4 `mpeg_quant` intra; q_scale_type = 0
/// so qscale is doubled).
///
/// * `scan`: the scan table in effect (ff `intra_scantable.scantable`, with identity IDCT
///   permutation); coefficients visited are `scan[1..=last_index]`. The AC-prediction
///   alternate scans are only used while parsing; stored coefficients are in raster order
///   and this function always uses the plain intra scan (zigzag / alternate-horizontal
///   selected by alternate_scan), as ffmpeg does.
/// * `block[0] *= dc_scale` (y or c dc scale) is done HERE (caller stores the DC level).
/// * `mismatch`: false = ffmpeg default (`dct_unquantize_mpeg2_intra_c`, no mismatch control;
///   ffmpeg only applies it with AV_CODEC_FLAG_BITEXACT); true = `_bitexact` variant:
///   sum starts at -1, `block[63] ^= sum & 1` where sum includes the scaled DC.
/// * `last_index` = block_last_index (>= 0 for intra, or -1 -> only DC handled).
pub fn unquant_mpeg2_intra(
    block: &mut [i16; 64],
    scan: &[u8; 64],
    last_index: i32,
    qscale: i32,
    matrix: &[u16; 64],
    dc_scale: i32,
    mismatch: bool,
) {
    let qscale = qscale << 1;
    block[0] = (block[0] as i32).wrapping_mul(dc_scale) as i16;
    let mut sum: i32 = -1 + block[0] as i32;
    let mut i = 1;
    while i as i32 <= last_index && i < 64 {
        let j = scan[i] as usize;
        let level = block[j] as i32;
        if level != 0 {
            let l = if level < 0 {
                -((-level).wrapping_mul(qscale).wrapping_mul(matrix[j] as i32) >> 4)
            } else {
                level.wrapping_mul(qscale).wrapping_mul(matrix[j] as i32) >> 4
            };
            block[j] = l as i16;
            sum = sum.wrapping_add(l);
        }
        i += 1;
    }
    if mismatch {
        block[63] ^= (sum & 1) as i16;
    }
}

/// dct_unquantize_mpeg2_inter_c (always has mismatch control: sum starts at -1,
/// `block[63] ^= sum & 1`). `scan` = intra scan table in effect (ffmpeg uses
/// intra_scantable here too), visits `scan[0..=last_index]`. ffmpeg only calls it when
/// last_index >= 0 (with last_index < 0 the C would still toggle block[63]; this port does too).
pub fn unquant_mpeg2_inter(
    block: &mut [i16; 64],
    scan: &[u8; 64],
    last_index: i32,
    qscale: i32,
    matrix: &[u16; 64],
) {
    let qscale = qscale << 1;
    let mut sum: i32 = -1;
    let mut i = 0;
    while i as i32 <= last_index && i < 64 {
        let j = scan[i] as usize;
        let level = block[j] as i32;
        if level != 0 {
            let f = |l: i32| {
                (((l << 1) + 1)
                    .wrapping_mul(qscale)
                    .wrapping_mul(matrix[j] as i32))
                    >> 5
            };
            let l = if level < 0 { -f(-level) } else { f(level) };
            block[j] = l as i16;
            sum = sum.wrapping_add(l);
        }
        i += 1;
    }
    block[63] ^= (sum & 1) as i16;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_dc_only() {
        for f in [idct_simple_put, idct_xvid_put] {
            let mut b = [0i16; 64];
            b[0] = 8 * 8; // DC 64 -> 8 per pixel after /8
            let mut d = [0u8; 8 * 16];
            f(&mut b, &mut d, 16);
            for r in 0..8 {
                for c in 0..8 {
                    assert_eq!(d[r * 16 + c], d[0], "{r} {c}");
                }
            }
            assert_eq!(d[0], 8);
        }
        let mut b = [0i16; 64];
        b[0] = 80;
        let mut d = [100u8; 64];
        idct_simple_add(&mut b, &mut d, 8);
        assert!(d.iter().all(|&x| x == 110));
        let mut b = [0i16; 64];
        b[0] = 80;
        let mut d = [100u8; 64];
        idct_xvid_add(&mut b, &mut d, 8);
        assert!(d.iter().all(|&x| x == 110));
    }

    fn pattern(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| ((i * 37 + (i / 7) * 11) & 0xff) as u8)
            .collect()
    }

    #[test]
    fn hpel_copy_and_avg() {
        let src = pattern(40 * 40);
        for &w in &[2usize, 4, 8, 16] {
            let mut d = vec![0u8; 40 * 40];
            put_hpel(&mut d, 40, &src, 40, w, 5, 0, false);
            for r in 0..5 {
                assert_eq!(&d[r * 40..r * 40 + w], &src[r * 40..r * 40 + w]);
            }
            let mut d = vec![0u8; 40 * 40];
            put_hpel(&mut d, 40, &src, 40, w, 5, 3, false);
            for r in 0..5 {
                for c in 0..w {
                    let s = |y: usize, x: usize| src[y * 40 + x] as u16;
                    let e = (s(r, c) + s(r, c + 1) + s(r + 1, c) + s(r + 1, c + 1) + 2) >> 2;
                    assert_eq!(d[r * 40 + c] as u16, e);
                }
            }
        }
        let mut d = vec![10u8; 40 * 40];
        avg_hpel(&mut d, 40, &src, 40, 16, 4, 1, false);
        let e = (((src[0] as u16 + src[1] as u16 + 1) >> 1) + 10 + 1) >> 1;
        assert_eq!(d[0] as u16, e);
    }

    #[test]
    fn qpel_mc00_copy_and_flat() {
        let src = pattern(40 * 40);
        for &sz in &[8usize, 16] {
            let mut d = vec![0u8; 40 * 40];
            put_qpel(&mut d, 40, &src, 40, sz, 0, false);
            for r in 0..sz {
                assert_eq!(&d[r * 40..r * 40 + sz], &src[r * 40..r * 40 + sz]);
            }
            // flat input stays flat for every mc, rnd and no_rnd
            let flat = vec![77u8; 40 * 40];
            for mc in 0..16 {
                for nr in [false, true] {
                    let mut d = vec![0u8; 40 * 40];
                    put_qpel(&mut d, 40, &flat, 40, sz, mc, nr);
                    for r in 0..sz {
                        assert!(
                            d[r * 40..r * 40 + sz].iter().all(|&x| x == 77),
                            "mc{mc} nr{nr}"
                        );
                    }
                }
                let mut d = vec![77u8; 40 * 40];
                avg_qpel(&mut d, 40, &flat, 40, sz, mc, false);
                assert!(d[..sz].iter().all(|&x| x == 77));
            }
        }
    }

    #[test]
    fn qpel_window_is_tight() {
        // must not read beyond (size+1)x(size+1)
        let src = pattern(17 * 17);
        for mc in 0..16 {
            let mut d = vec![0u8; 16 * 16];
            put_qpel(&mut d, 16, &src, 17, 16, mc, false);
        }
    }

    #[test]
    fn gmc1_identity() {
        let src = pattern(16 * 16);
        let mut d = vec![0u8; 16 * 16];
        gmc1(&mut d, 16, &src, 16, 8, 0, 0, 128);
        for r in 0..8 {
            assert_eq!(&d[r * 16..r * 16 + 8], &src[r * 16..r * 16 + 8]);
        }
    }

    #[test]
    fn gmc_identity() {
        let src = pattern(32 * 32);
        let mut d = vec![0u8; 8 * 8];
        // integer position (4,4), no warp, shift 4 (s=16)
        gmc(
            &mut d,
            8,
            &src,
            32,
            8,
            4 << 20,
            4 << 20,
            1 << 20,
            0,
            0,
            1 << 20,
            4,
            128,
            32,
            32,
        );
        for r in 0..8 {
            assert_eq!(
                &d[r * 8..r * 8 + 8],
                &src[(r + 4) * 32 + 4..(r + 4) * 32 + 12]
            );
        }
    }
}

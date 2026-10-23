//! The 8x8 inverse DCT: a bit-exact integer implementation of the "simple"
//! IDCT (row pass with the DC-only shortcut, then column pass), the one the
//! reference decoder we match uses by default. Rows first, results of the row
//! pass stored as 16-bit.

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
fn row_pass(row: &mut [i16]) {
    let r: [i32; 8] = std::array::from_fn(|i| row[i] as i32);
    if r[1] == 0 && r[2] == 0 && r[3] == 0 && r[4] == 0 && r[5] == 0 && r[6] == 0 && r[7] == 0 {
        let t = (r[0].wrapping_mul(8)) as i16;
        row.fill(t);
        return;
    }
    let mut a0 = W4.wrapping_mul(r[0]).wrapping_add(1 << (ROW_SHIFT - 1));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 = a0.wrapping_add(W2 * r[2]);
    a1 = a1.wrapping_add(W6 * r[2]);
    a2 = a2.wrapping_sub(W6 * r[2]);
    a3 = a3.wrapping_sub(W2 * r[2]);
    let mut b0 = W1 * r[1] + W3 * r[3];
    let mut b1 = W3 * r[1] - W7 * r[3];
    let mut b2 = W5 * r[1] - W1 * r[3];
    let mut b3 = W7 * r[1] - W5 * r[3];
    if r[4] != 0 || r[5] != 0 || r[6] != 0 || r[7] != 0 {
        a0 = a0.wrapping_add(W4 * r[4] + W6 * r[6]);
        a1 = a1.wrapping_add(-W4 * r[4] - W2 * r[6]);
        a2 = a2.wrapping_add(-W4 * r[4] + W2 * r[6]);
        a3 = a3.wrapping_add(W4 * r[4] - W6 * r[6]);
        b0 += W5 * r[5] + W7 * r[7];
        b1 += -W1 * r[5] - W5 * r[7];
        b2 += W7 * r[5] + W3 * r[7];
        b3 += W3 * r[5] - W1 * r[7];
    }
    row[0] = (a0.wrapping_add(b0) >> ROW_SHIFT) as i16;
    row[7] = (a0.wrapping_sub(b0) >> ROW_SHIFT) as i16;
    row[1] = (a1.wrapping_add(b1) >> ROW_SHIFT) as i16;
    row[6] = (a1.wrapping_sub(b1) >> ROW_SHIFT) as i16;
    row[2] = (a2.wrapping_add(b2) >> ROW_SHIFT) as i16;
    row[5] = (a2.wrapping_sub(b2) >> ROW_SHIFT) as i16;
    row[3] = (a3.wrapping_add(b3) >> ROW_SHIFT) as i16;
    row[4] = (a3.wrapping_sub(b3) >> ROW_SHIFT) as i16;
}

/// Column pass for column `x`, returning the eight outputs top to bottom.
#[inline(always)]
fn col_pass(blk: &[i16; 64], x: usize) -> [i32; 8] {
    let c = |k: usize| blk[8 * k + x] as i32;
    let mut a0 = W4.wrapping_mul(c(0).wrapping_add((1 << (COL_SHIFT - 1)) / W4));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 = a0.wrapping_add(W2.wrapping_mul(c(2)));
    a1 = a1.wrapping_add(W6.wrapping_mul(c(2)));
    a2 = a2.wrapping_sub(W6.wrapping_mul(c(2)));
    a3 = a3.wrapping_sub(W2.wrapping_mul(c(2)));
    let mut b0 = W1.wrapping_mul(c(1)).wrapping_add(W3.wrapping_mul(c(3)));
    let mut b1 = W3.wrapping_mul(c(1)).wrapping_sub(W7.wrapping_mul(c(3)));
    let mut b2 = W5.wrapping_mul(c(1)).wrapping_sub(W1.wrapping_mul(c(3)));
    let mut b3 = W7.wrapping_mul(c(1)).wrapping_sub(W5.wrapping_mul(c(3)));
    let c4 = c(4);
    if c4 != 0 {
        let t = W4.wrapping_mul(c4);
        a0 = a0.wrapping_add(t);
        a1 = a1.wrapping_sub(t);
        a2 = a2.wrapping_sub(t);
        a3 = a3.wrapping_add(t);
    }
    let c5 = c(5);
    if c5 != 0 {
        b0 = b0.wrapping_add(W5.wrapping_mul(c5));
        b1 = b1.wrapping_sub(W1.wrapping_mul(c5));
        b2 = b2.wrapping_add(W7.wrapping_mul(c5));
        b3 = b3.wrapping_add(W3.wrapping_mul(c5));
    }
    let c6 = c(6);
    if c6 != 0 {
        a0 = a0.wrapping_add(W6.wrapping_mul(c6));
        a1 = a1.wrapping_sub(W2.wrapping_mul(c6));
        a2 = a2.wrapping_add(W2.wrapping_mul(c6));
        a3 = a3.wrapping_sub(W6.wrapping_mul(c6));
    }
    let c7 = c(7);
    if c7 != 0 {
        b0 = b0.wrapping_add(W7.wrapping_mul(c7));
        b1 = b1.wrapping_sub(W5.wrapping_mul(c7));
        b2 = b2.wrapping_add(W3.wrapping_mul(c7));
        b3 = b3.wrapping_sub(W1.wrapping_mul(c7));
    }
    [
        a0.wrapping_add(b0) >> COL_SHIFT,
        a1.wrapping_add(b1) >> COL_SHIFT,
        a2.wrapping_add(b2) >> COL_SHIFT,
        a3.wrapping_add(b3) >> COL_SHIFT,
        a3.wrapping_sub(b3) >> COL_SHIFT,
        a2.wrapping_sub(b2) >> COL_SHIFT,
        a1.wrapping_sub(b1) >> COL_SHIFT,
        a0.wrapping_sub(b0) >> COL_SHIFT,
    ]
}

#[inline(always)]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Inverse transform `blk` and store the clamped result at `dst` (row pitch `stride`).
pub(crate) fn idct_put(blk: &mut [i16; 64], dst: &mut [u8], stride: usize) {
    for r in 0..8 {
        row_pass(&mut blk[8 * r..8 * r + 8]);
    }
    for x in 0..8 {
        let o = col_pass(blk, x);
        for (y, v) in o.iter().enumerate() {
            dst[y * stride + x] = clip(*v);
        }
    }
}

/// Inverse transform `blk` and add it to the samples at `dst`.
pub(crate) fn idct_add(blk: &mut [i16; 64], dst: &mut [u8], stride: usize) {
    for r in 0..8 {
        row_pass(&mut blk[8 * r..8 * r + 8]);
    }
    for x in 0..8 {
        let o = col_pass(blk, x);
        for (y, v) in o.iter().enumerate() {
            let p = &mut dst[y * stride + x];
            *p = clip(*p as i32 + *v);
        }
    }
}

/// Block with only a DC coefficient: every output sample is the same.
pub(crate) fn dc_value(dc: i16) -> i32 {
    let r = (dc as i32).wrapping_mul(8) as i16 as i32;
    W4.wrapping_mul(r.wrapping_add((1 << (COL_SHIFT - 1)) / W4)) >> COL_SHIFT
}

pub(crate) fn idct_put_dc(dc: i16, dst: &mut [u8], stride: usize) {
    let v = clip(dc_value(dc));
    for y in 0..8 {
        dst[y * stride..y * stride + 8].fill(v);
    }
}

pub(crate) fn idct_add_dc(dc: i16, dst: &mut [u8], stride: usize) {
    let v = dc_value(dc);
    for y in 0..8 {
        for p in &mut dst[y * stride..y * stride + 8] {
            *p = clip(*p as i32 + v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_shortcut_matches_full_transform() {
        for dc in [-2048i16, -1100, -3, 0, 1, 77, 1024, 2047] {
            let mut blk = [0i16; 64];
            blk[0] = dc;
            let mut a = [0u8; 64];
            idct_put(&mut blk, &mut a, 8);
            let mut b = [0u8; 64];
            idct_put_dc(dc, &mut b, 8);
            assert_eq!(a, b, "dc {dc}");
        }
    }
}

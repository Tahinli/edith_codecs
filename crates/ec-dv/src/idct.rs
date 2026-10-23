//! FFmpeg's integer "simple" IDCT (8-bit, `ff_simple_idct_put_int16_8bit`) and DV's 2-4-8 IDCT
//! (`ff_simple_idct248_put`), reproduced including the 16-bit intermediate wrap.

// W1..W7 are round(2^14 * sqrt(2) * cos(k*pi/16)) (the Chen-Wang scaling used by the MPEG-2
// reference "simple" IDCT; ISO/IEC 13818-2 / ITU-T T.81 give the underlying cosine kernel),
// except W4, which is 16383 rather than 16384. Checked by `constants_follow_formula`.
const W1: i32 = 22725;
const W2: i32 = 21407;
const W3: i32 = 19266;
const W4: i32 = 16383;
const W5: i32 = 12873;
const W6: i32 = 8867;
const W7: i32 = 4520;
const ROW_SHIFT: u32 = 11;
const COL_SHIFT: u32 = 20;
const DC_SHIFT: u32 = 3;

#[inline(always)]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

#[inline(always)]
fn row_idct(row: &mut [i16]) {
    let r: [i32; 8] = std::array::from_fn(|i| i32::from(row[i]));
    if r[1] | r[2] | r[3] | r[4] | r[5] | r[6] | r[7] == 0 {
        let t = (r[0].wrapping_mul(1 << DC_SHIFT)) as i16;
        row.fill(t);
        return;
    }
    let mut a0 = W4.wrapping_mul(r[0]).wrapping_add(1 << (ROW_SHIFT - 1));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 = a0.wrapping_add(W2.wrapping_mul(r[2]));
    a1 = a1.wrapping_add(W6.wrapping_mul(r[2]));
    a2 = a2.wrapping_sub(W6.wrapping_mul(r[2]));
    a3 = a3.wrapping_sub(W2.wrapping_mul(r[2]));
    let mut b0 = W1.wrapping_mul(r[1]).wrapping_add(W3.wrapping_mul(r[3]));
    let mut b1 = W3.wrapping_mul(r[1]).wrapping_sub(W7.wrapping_mul(r[3]));
    let mut b2 = W5.wrapping_mul(r[1]).wrapping_sub(W1.wrapping_mul(r[3]));
    let mut b3 = W7.wrapping_mul(r[1]).wrapping_sub(W5.wrapping_mul(r[3]));
    a0 = a0.wrapping_add(W4.wrapping_mul(r[4]).wrapping_add(W6.wrapping_mul(r[6])));
    a1 = a1.wrapping_add((-W4).wrapping_mul(r[4]).wrapping_sub(W2.wrapping_mul(r[6])));
    a2 = a2.wrapping_add((-W4).wrapping_mul(r[4]).wrapping_add(W2.wrapping_mul(r[6])));
    a3 = a3.wrapping_add(W4.wrapping_mul(r[4]).wrapping_sub(W6.wrapping_mul(r[6])));
    b0 = b0.wrapping_add(W5.wrapping_mul(r[5])).wrapping_add(W7.wrapping_mul(r[7]));
    b1 = b1.wrapping_sub(W1.wrapping_mul(r[5])).wrapping_sub(W5.wrapping_mul(r[7]));
    b2 = b2.wrapping_add(W7.wrapping_mul(r[5])).wrapping_add(W3.wrapping_mul(r[7]));
    b3 = b3.wrapping_add(W3.wrapping_mul(r[5])).wrapping_sub(W1.wrapping_mul(r[7]));
    row[0] = (a0.wrapping_add(b0) >> ROW_SHIFT) as i16;
    row[7] = (a0.wrapping_sub(b0) >> ROW_SHIFT) as i16;
    row[1] = (a1.wrapping_add(b1) >> ROW_SHIFT) as i16;
    row[6] = (a1.wrapping_sub(b1) >> ROW_SHIFT) as i16;
    row[2] = (a2.wrapping_add(b2) >> ROW_SHIFT) as i16;
    row[5] = (a2.wrapping_sub(b2) >> ROW_SHIFT) as i16;
    row[3] = (a3.wrapping_add(b3) >> ROW_SHIFT) as i16;
    row[4] = (a3.wrapping_sub(b3) >> ROW_SHIFT) as i16;
}

#[inline(always)]
fn col_ab(col: impl Fn(usize) -> i32) -> ([i32; 4], [i32; 4]) {
    let mut a0 = W4.wrapping_mul(col(0).wrapping_add((1 << (COL_SHIFT - 1)) / W4));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 = a0.wrapping_add(W2.wrapping_mul(col(2)));
    a1 = a1.wrapping_add(W6.wrapping_mul(col(2)));
    a2 = a2.wrapping_sub(W6.wrapping_mul(col(2)));
    a3 = a3.wrapping_sub(W2.wrapping_mul(col(2)));
    let mut b0 = W1.wrapping_mul(col(1)).wrapping_add(W3.wrapping_mul(col(3)));
    let mut b1 = W3.wrapping_mul(col(1)).wrapping_sub(W7.wrapping_mul(col(3)));
    let mut b2 = W5.wrapping_mul(col(1)).wrapping_sub(W1.wrapping_mul(col(3)));
    let mut b3 = W7.wrapping_mul(col(1)).wrapping_sub(W5.wrapping_mul(col(3)));
    a0 = a0.wrapping_add(W4.wrapping_mul(col(4)));
    a1 = a1.wrapping_sub(W4.wrapping_mul(col(4)));
    a2 = a2.wrapping_sub(W4.wrapping_mul(col(4)));
    a3 = a3.wrapping_add(W4.wrapping_mul(col(4)));
    b0 = b0.wrapping_add(W5.wrapping_mul(col(5)));
    b1 = b1.wrapping_sub(W1.wrapping_mul(col(5)));
    b2 = b2.wrapping_add(W7.wrapping_mul(col(5)));
    b3 = b3.wrapping_add(W3.wrapping_mul(col(5)));
    a0 = a0.wrapping_add(W6.wrapping_mul(col(6)));
    a1 = a1.wrapping_sub(W2.wrapping_mul(col(6)));
    a2 = a2.wrapping_add(W2.wrapping_mul(col(6)));
    a3 = a3.wrapping_sub(W6.wrapping_mul(col(6)));
    b0 = b0.wrapping_add(W7.wrapping_mul(col(7)));
    b1 = b1.wrapping_sub(W5.wrapping_mul(col(7)));
    b2 = b2.wrapping_add(W3.wrapping_mul(col(7)));
    b3 = b3.wrapping_sub(W1.wrapping_mul(col(7)));
    ([a0, a1, a2, a3], [b0, b1, b2, b3])
}

#[inline(always)]
fn col_idct(block: &[i16; 64], c: usize) -> [u8; 8] {
    let (a, b) = col_ab(|k| i32::from(block[8 * k + c]));
    [
        clip8(a[0].wrapping_add(b[0]) >> COL_SHIFT),
        clip8(a[1].wrapping_add(b[1]) >> COL_SHIFT),
        clip8(a[2].wrapping_add(b[2]) >> COL_SHIFT),
        clip8(a[3].wrapping_add(b[3]) >> COL_SHIFT),
        clip8(a[3].wrapping_sub(b[3]) >> COL_SHIFT),
        clip8(a[2].wrapping_sub(b[2]) >> COL_SHIFT),
        clip8(a[1].wrapping_sub(b[1]) >> COL_SHIFT),
        clip8(a[0].wrapping_sub(b[0]) >> COL_SHIFT),
    ]
}

/// Column kernel for blocks whose rows 4..8 are zero: identical result, fewer terms.
#[inline(always)]
fn col_idct_low(block: &[i16; 64], c: usize) -> [u8; 8] {
    let col = |k: usize| i32::from(block[8 * k + c]);
    let a0 = W4.wrapping_mul(col(0).wrapping_add((1 << (COL_SHIFT - 1)) / W4));
    let (c1, c2, c3) = (col(1), col(2), col(3));
    let a0n = a0.wrapping_add(W2.wrapping_mul(c2));
    let a1 = a0.wrapping_add(W6.wrapping_mul(c2));
    let a2 = a0.wrapping_sub(W6.wrapping_mul(c2));
    let a3 = a0.wrapping_sub(W2.wrapping_mul(c2));
    let b0 = W1.wrapping_mul(c1).wrapping_add(W3.wrapping_mul(c3));
    let b1 = W3.wrapping_mul(c1).wrapping_sub(W7.wrapping_mul(c3));
    let b2 = W5.wrapping_mul(c1).wrapping_sub(W1.wrapping_mul(c3));
    let b3 = W7.wrapping_mul(c1).wrapping_sub(W5.wrapping_mul(c3));
    [
        clip8(a0n.wrapping_add(b0) >> COL_SHIFT),
        clip8(a1.wrapping_add(b1) >> COL_SHIFT),
        clip8(a2.wrapping_add(b2) >> COL_SHIFT),
        clip8(a3.wrapping_add(b3) >> COL_SHIFT),
        clip8(a3.wrapping_sub(b3) >> COL_SHIFT),
        clip8(a2.wrapping_sub(b2) >> COL_SHIFT),
        clip8(a1.wrapping_sub(b1) >> COL_SHIFT),
        clip8(a0n.wrapping_sub(b0) >> COL_SHIFT),
    ]
}

/// `ff_simple_idct_int16_8bit`: in-place transform, results stored as `i16` without clamping.
pub fn idct_i16(block: &mut [i16; 64]) {
    for r in 0..8 {
        row_idct(&mut block[r * 8..r * 8 + 8]);
    }
    for c in 0..8 {
        let col = |k: usize| i32::from(block[8 * k + c]);
        let (a, b) = col_ab(col);
        let o = [
            a[0].wrapping_add(b[0]), a[1].wrapping_add(b[1]), a[2].wrapping_add(b[2]), a[3].wrapping_add(b[3]),
            a[3].wrapping_sub(b[3]), a[2].wrapping_sub(b[2]), a[1].wrapping_sub(b[1]), a[0].wrapping_sub(b[0]),
        ];
        for (k, v) in o.iter().enumerate() {
            block[8 * k + c] = (v >> COL_SHIFT) as i16;
        }
    }
}

/// Row pass shared by every transform: returns whether rows 4..8 were all zero on entry
/// (zero rows stay zero, so the column pass can skip their terms exactly).
#[inline(always)]
fn row_pass(block: &mut [i16; 64]) -> bool {
    let low = block[32..].iter().all(|&v| v == 0);
    let rows = if low { 4 } else { 8 };
    for r in 0..rows {
        let row = &mut block[r * 8..r * 8 + 8];
        let any = row.iter().fold(0i16, |x, &v| x | v);
        if any == 0 {
            continue;
        }
        row_idct(row);
    }
    low
}

/// Inverse transform an 8x8 block (levels biased by +1024) to 8x8 pixels, row-major.
pub fn idct_tile(block: &mut [i16; 64]) -> [u8; 64] {
    let low = row_pass(block);
    let mut px = [0u8; 64];
    let blk: &[i16; 64] = block;
    if low {
        for c in 0..8 {
            let col = col_idct_low(blk, c);
            px[c] = col[0];
            px[8 + c] = col[1];
            px[16 + c] = col[2];
            px[24 + c] = col[3];
            px[32 + c] = col[4];
            px[40 + c] = col[5];
            px[48 + c] = col[6];
            px[56 + c] = col[7];
        }
    } else {
        for c in 0..8 {
            let col = col_idct(blk, c);
            px[c] = col[0];
            px[8 + c] = col[1];
            px[16 + c] = col[2];
            px[24 + c] = col[3];
            px[32 + c] = col[4];
            px[40 + c] = col[5];
            px[48 + c] = col[6];
            px[56 + c] = col[7];
        }
    }
    px
}

const CN_SHIFT: u32 = 12;
const C1: i32 = 2676;
const C2: i32 = 1108;
const C_SHIFT: u32 = 4 + 1 + 12;

#[inline(always)]
fn idct4col(block: &[i16; 64], base: usize, c: usize) -> [u8; 4] {
    let a0 = i32::from(block[base + c]);
    let a1 = i32::from(block[base + 16 + c]);
    let a2 = i32::from(block[base + 32 + c]);
    let a3 = i32::from(block[base + 48 + c]);
    let c0 = ((a0 + a2) * (1 << (CN_SHIFT - 1))) + (1 << (C_SHIFT - 1));
    let c2 = ((a0 - a2) * (1 << (CN_SHIFT - 1))) + (1 << (C_SHIFT - 1));
    let c1 = a1 * C1 + a3 * C2;
    let c3 = a1 * C2 - a3 * C1;
    [
        clip8((c0 + c1) >> C_SHIFT),
        clip8((c2 + c3) >> C_SHIFT),
        clip8((c2 - c3) >> C_SHIFT),
        clip8((c0 - c1) >> C_SHIFT),
    ]
}

/// DV's 2-4-8 transform for field-mode blocks: the block rows hold the two fields interleaved;
/// the result is the 8x8 pixel tile in display order (even tile rows from block rows 0,2,4,6).
pub fn idct248_tile(block: &mut [i16; 64]) -> [u8; 64] {
    for i in 0..4 {
        for k in 0..8 {
            let a0 = block[i * 16 + k];
            let a1 = block[i * 16 + 8 + k];
            block[i * 16 + k] = a0.wrapping_add(a1);
            block[i * 16 + 8 + k] = a0.wrapping_sub(a1);
        }
    }
    for r in 0..8 {
        row_idct(&mut block[r * 8..r * 8 + 8]);
    }
    let mut px = [0u8; 64];
    for c in 0..8 {
        let even = idct4col(block, 0, c);
        let odd = idct4col(block, 8, c);
        for k in 0..4 {
            px[2 * k * 8 + c] = even[k];
            px[(2 * k + 1) * 8 + c] = odd[k];
        }
    }
    px
}

#[cfg(test)]
mod const_tests {
    use super::*;

    #[test]
    fn constants_follow_formula() {
        let w = [W1, W2, W3, W4, W5, W6, W7];
        for (i, &v) in w.iter().enumerate() {
            let k = (i + 1) as f64;
            let exact = 16384.0 * 2f64.sqrt() * (k * std::f64::consts::PI / 16.0).cos();
            let want = if i == 3 { 16383 } else { exact.round() as i32 };
            assert_eq!(v, want, "W{}", i + 1);
        }
    }
}

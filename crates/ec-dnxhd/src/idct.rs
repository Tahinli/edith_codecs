//! The reference decoder's "simple" integer IDCT, 16-bit coefficient flavour,
//! for 8, 10 and 12-bit output: row pass with the DC-only shortcut, results of
//! the row pass kept as 16-bit, then the column pass with clipping.

#[inline(always)]
const fn consts<const BD: u32>() -> [i32; 10] {
    // W1..W7, ROW_SHIFT, COL_SHIFT, DC_SHIFT
    match BD {
        8 => [22725, 21407, 19266, 16383, 12873, 8867, 4520, 11, 20, 3],
        10 => [22725, 21407, 19265, 16384, 12873, 8867, 4520, 12, 19, 2],
        _ => [45451, 42813, 38531, 32767, 25746, 17734, 9041, 16, 17, -1],
    }
}

pub(crate) type Block = [[i16; 8]; 8];

#[inline(always)]
fn row_pass<const BD: u32>(row: &mut [i16; 8]) {
    let [w1, w2, w3, w4, w5, w6, w7, row_shift, _, dc_shift] = consts::<BD>();
    let r: [i32; 8] = std::array::from_fn(|i| row[i] as i32);
    if r[1..].iter().all(|&v| v == 0) {
        let t = if dc_shift >= 0 {
            r[0].wrapping_shl(dc_shift as u32)
        } else {
            (r[0] + (1 << (-dc_shift - 1))) >> -dc_shift
        } as i16;
        *row = [t; 8];
        return;
    }
    let m = |a: i32, b: i32| a.wrapping_mul(b);
    let mut a0 = m(w4, r[0]).wrapping_add(1 << (row_shift - 1));
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    a0 = a0.wrapping_add(m(w2, r[2]));
    a1 = a1.wrapping_add(m(w6, r[2]));
    a2 = a2.wrapping_sub(m(w6, r[2]));
    a3 = a3.wrapping_sub(m(w2, r[2]));
    let mut b0 = m(w1, r[1]).wrapping_add(m(w3, r[3]));
    let mut b1 = m(w3, r[1]).wrapping_add(m(-w7, r[3]));
    let mut b2 = m(w5, r[1]).wrapping_add(m(-w1, r[3]));
    let mut b3 = m(w7, r[1]).wrapping_add(m(-w5, r[3]));
    if r[4] != 0 || r[5] != 0 || r[6] != 0 || r[7] != 0 {
        a0 = a0.wrapping_add(m(w4, r[4]).wrapping_add(m(w6, r[6])));
        a1 = a1.wrapping_add(m(-w4, r[4]).wrapping_sub(m(w2, r[6])));
        a2 = a2.wrapping_add(m(-w4, r[4]).wrapping_add(m(w2, r[6])));
        a3 = a3.wrapping_add(m(w4, r[4]).wrapping_sub(m(w6, r[6])));
        b0 = b0.wrapping_add(m(w5, r[5])).wrapping_add(m(w7, r[7]));
        b1 = b1.wrapping_add(m(-w1, r[5])).wrapping_add(m(-w5, r[7]));
        b2 = b2.wrapping_add(m(w7, r[5])).wrapping_add(m(w3, r[7]));
        b3 = b3.wrapping_add(m(w3, r[5])).wrapping_add(m(-w1, r[7]));
    }
    row[0] = (a0.wrapping_add(b0) >> row_shift) as i16;
    row[7] = (a0.wrapping_sub(b0) >> row_shift) as i16;
    row[1] = (a1.wrapping_add(b1) >> row_shift) as i16;
    row[6] = (a1.wrapping_sub(b1) >> row_shift) as i16;
    row[2] = (a2.wrapping_add(b2) >> row_shift) as i16;
    row[5] = (a2.wrapping_sub(b2) >> row_shift) as i16;
    row[3] = (a3.wrapping_add(b3) >> row_shift) as i16;
    row[4] = (a3.wrapping_sub(b3) >> row_shift) as i16;
}

/// Column `c` of the second pass, results into `out[row][c]`.
#[inline(always)]
fn col_pass<const BD: u32, const LOW: bool>(blk: &Block, c: usize, out: &mut [[u16; 8]; 8]) {
    let [w1, w2, w3, w4, w5, w6, w7, _, col_shift, _] = consts::<BD>();
    let m = |a: i32, b: i32| a.wrapping_mul(b);
    // LOW: the coefficients live in rows 0..3, the lower half is zero.
    let v = |k: usize| if LOW && k >= 4 { 0 } else { blk[k][c] as i32 };
    let max = (1i32 << BD) - 1;
    let clip = |x: i32| (x >> col_shift).max(0).min(max) as u16;
    let a0 = m(w4, v(0) + ((1 << (col_shift - 1)) / w4));
    let (v1, v2, v3) = (v(1), v(2), v(3));
    let (v4, v5, v6, v7) = (v(4), v(5), v(6), v(7));
    if v1 == 0 && v2 == 0 && v3 == 0 && v4 == 0 && v5 == 0 && v6 == 0 && v7 == 0 {
        // Only the DC term: every output is the same value.
        let x = clip(a0);
        for row in out.iter_mut() {
            row[c] = x;
        }
        return;
    }
    let mut a1 = a0;
    let mut a2 = a0;
    let mut a3 = a0;
    let mut a0 = a0;
    a0 = a0.wrapping_add(m(w2, v2));
    a1 = a1.wrapping_add(m(w6, v2));
    a2 = a2.wrapping_add(m(-w6, v2));
    a3 = a3.wrapping_add(m(-w2, v2));
    let mut b0 = m(w1, v1).wrapping_add(m(w3, v3));
    let mut b1 = m(w3, v1).wrapping_add(m(-w7, v3));
    let mut b2 = m(w5, v1).wrapping_add(m(-w1, v3));
    let mut b3 = m(w7, v1).wrapping_add(m(-w5, v3));
    if !LOW {
        a0 = a0.wrapping_add(m(w4, v4)).wrapping_add(m(w6, v6));
        a1 = a1.wrapping_add(m(-w4, v4)).wrapping_add(m(-w2, v6));
        a2 = a2.wrapping_add(m(-w4, v4)).wrapping_add(m(w2, v6));
        a3 = a3.wrapping_add(m(w4, v4)).wrapping_add(m(-w6, v6));
        b0 = b0.wrapping_add(m(w5, v5)).wrapping_add(m(w7, v7));
        b1 = b1.wrapping_add(m(-w1, v5)).wrapping_add(m(-w5, v7));
        b2 = b2.wrapping_add(m(w7, v5)).wrapping_add(m(w3, v7));
        b3 = b3.wrapping_add(m(w3, v5)).wrapping_add(m(-w1, v7));
    }
    out[0][c] = clip(a0.wrapping_add(b0));
    out[1][c] = clip(a1.wrapping_add(b1));
    out[2][c] = clip(a2.wrapping_add(b2));
    out[3][c] = clip(a3.wrapping_add(b3));
    out[4][c] = clip(a3.wrapping_sub(b3));
    out[5][c] = clip(a2.wrapping_sub(b2));
    out[6][c] = clip(a1.wrapping_sub(b1));
    out[7][c] = clip(a0.wrapping_sub(b0));
}

/// Inverse transform `blk`, whose last nonzero coefficient is at scan
/// position `last`, and store the clipped 8x8 result at `dst[0..]`, rows
/// `stride` samples apart.
pub(crate) fn idct_put<const BD: u32>(blk: &mut Block, dst: &mut [u8], stride: usize, last: usize) {
    let mut out = [[0u16; 8]; 8];
    if last == 0 {
        // Row 0 spreads the DC term, the other rows are zero and stay zero, so
        // every column holds the same lone value: one result for all 64 pixels.
        let [_, _, _, w4, _, _, _, _, col_shift, dc_shift] = consts::<BD>();
        let r0 = blk[0][0] as i32;
        let t = if dc_shift >= 0 {
            r0.wrapping_shl(dc_shift as u32)
        } else {
            (r0 + (1 << (-dc_shift - 1))) >> -dc_shift
        } as i16 as i32;
        let a0 = w4.wrapping_mul(t + ((1 << (col_shift - 1)) / w4));
        let x = (a0 >> col_shift).max(0).min((1i32 << BD) - 1) as u16;
        out = [[x; 8]; 8];
    } else if last <= 9 {
        // Scan positions 0..=9 all lie in rows 0..3 and columns 0..3.
        for r in blk.iter_mut().take(4) {
            row_pass::<BD>(r);
        }
        for c in 0..8 {
            col_pass::<BD, true>(blk, c, &mut out);
        }
    } else {
        for r in blk.iter_mut() {
            row_pass::<BD>(r);
        }
        for c in 0..8 {
            col_pass::<BD, false>(blk, c, &mut out);
        }
    }
    // `dst` holds one byte per sample for 8-bit output, two (little endian) above.
    if BD == 8 {
        for (row, o) in dst.chunks_mut(stride).zip(out.iter()) {
            for (d, &v) in row[..8].iter_mut().zip(o) {
                *d = v as u8;
            }
        }
    } else {
        for (row, o) in dst.chunks_mut(stride).zip(out.iter()) {
            for (d, &v) in row[..16].chunks_exact_mut(2).zip(o) {
                d.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

pub(crate) type IdctPut = fn(&mut Block, &mut [u8], usize, usize);

pub(crate) fn idct_for(bit_depth: u32) -> IdctPut {
    match bit_depth {
        8 => idct_put::<8>,
        10 => idct_put::<10>,
        _ => idct_put::<12>,
    }
}

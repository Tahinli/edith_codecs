//! The inverse transform (spec 7.13), so the encoder can reconstruct exactly
//! what a decoder will show.
//!
//! Only the inverse DCT is here, which is all this encoder's subset codes: a
//! square `DCT_DCT` transform per block. The butterfly network is the spec's
//! own, stage for stage — every stage's rounding and intermediate clamp is
//! part of the bitstream contract, so a mathematically equal but differently
//! rounded DCT would drift away from the decoder by a sample here and there.

/// `Cos128_Lookup` (spec 7.13.2.1): `4096 * cos(angle * pi / 128)` rounded.
static COS128_LOOKUP: [i32; 65] = [
    4096, 4095, 4091, 4085, 4076, 4065, 4052, 4036, 4017, 3996, 3973, 3948, 3920, 3889, 3857, 3822,
    3784, 3745, 3703, 3659, 3612, 3564, 3513, 3461, 3406, 3349, 3290, 3229, 3166, 3102, 3035, 2967,
    2896, 2824, 2751, 2675, 2598, 2520, 2440, 2359, 2276, 2191, 2106, 2019, 1931, 1842, 1751, 1660,
    1567, 1474, 1380, 1285, 1189, 1092, 995, 897, 799, 700, 601, 501, 401, 301, 201, 101, 0,
];

/// The fixed-point position of the cosine table.
const COS_BITS: usize = 12;

/// `cos128(angle)` (spec 7.13.2.1), folded from the quarter table.
fn cos128(angle: i32) -> i32 {
    let angle2 = (angle & 255) as usize;
    match angle2 {
        0..=64 => COS128_LOOKUP[angle2],
        65..=128 => -COS128_LOOKUP[128 - angle2],
        129..=192 => -COS128_LOOKUP[angle2 - 128],
        _ => COS128_LOOKUP[256 - angle2],
    }
}

/// `sin128(angle)` (spec 7.13.2.1).
fn sin128(angle: i32) -> i32 {
    cos128(angle - 64)
}

/// `brev(numBits, x)` (spec 7.13.2.1): the bit reversal of the low `num_bits`.
fn brev(num_bits: u32, x: usize) -> usize {
    (0..num_bits).fold(0, |t, i| t | (((x >> i) & 1) << (num_bits - 1 - i)))
}

/// `Round2(x, n)` (spec 4.7): shift down `n` places, rounding halves up.
fn round2(x: i32, n: usize) -> i32 {
    if n == 0 {
        x
    } else {
        // The intermediate can exceed 32 bits before the shift brings it
        // back, which is why it is done in 64.
        ((i64::from(x) + (1i64 << (n - 1))) >> n) as i32
    }
}

/// `Clip3` against a signed integer of `r` bits, which is what bounds every
/// intermediate of the transform.
fn clamp_range(x: i32, r: usize) -> i32 {
    let hi = ((1i64 << (r - 1)) - 1) as i32;
    let lo = (-(1i64 << (r - 1))) as i32;
    x.clamp(lo, hi)
}

/// `B(a, b, angle, flip, r)` (spec 7.13.2.1): a butterfly rotation, and an
/// exchange of the two entries when `flip` is set.
fn butterfly(t: &mut [i32], a: usize, b: usize, angle: i32, flip: bool, _r: usize) {
    let (ta, tb) = (i64::from(t[a]), i64::from(t[b]));
    let (c, s) = (i64::from(cos128(angle)), i64::from(sin128(angle)));
    let x = ta * c - tb * s;
    let y = ta * s + tb * c;
    t[a] = ((x + (1 << (COS_BITS - 1))) >> COS_BITS) as i32;
    t[b] = ((y + (1 << (COS_BITS - 1))) >> COS_BITS) as i32;
    if flip {
        t.swap(a, b);
    }
}

/// `H(a, b, flip, r)` (spec 7.13.2.1): a Hadamard rotation, with the indices
/// exchanged when `flip` is set.
fn hadamard(t: &mut [i32], a: usize, b: usize, flip: bool, r: usize) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let (x, y) = (t[a], t[b]);
    t[a] = clamp_range(x.wrapping_add(y), r);
    t[b] = clamp_range(x.wrapping_sub(y), r);
}

/// The inverse DCT array permutation (spec 7.13.2.2): an in-place bit-reversal
/// of the first `1 << n` entries.
fn permute(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let copy: Vec<i32> = t[..n0].to_vec();
    for (i, v) in t[..n0].iter_mut().enumerate() {
        *v = copy[brev(n, i)];
    }
}

/// The inverse DCT process (spec 7.13.2.3): an in-place inverse DCT of the
/// first `1 << n` entries of `t`, clamping intermediates to `r` bits.
///
/// The stage list is the spec's, in the spec's order; the guards on `n` are
/// what make one network serve every transform size from 4 to 64.
pub fn inverse_dct(t: &mut [i32], n: u32, r: usize) {
    assert!((2..=6).contains(&n), "the inverse DCT is defined for 4..64");
    permute(t, n);

    if n == 6 {
        for i in 0..16 {
            butterfly(t, 32 + i, 63 - i, 63 - 4 * brev(4, i) as i32, false, r);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            butterfly(
                t,
                16 + i,
                31 - i,
                6 + ((brev(3, 7 - i) as i32) << 3),
                false,
                r,
            );
        }
    }
    if n == 6 {
        for i in 0..16 {
            hadamard(t, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            butterfly(
                t,
                8 + i,
                15 - i,
                12 + ((brev(2, 3 - i) as i32) << 4),
                false,
                r,
            );
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hadamard(t, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                let angle = 60 - 16 * brev(2, i) as i32 + 64 * j as i32;
                butterfly(t, 62 - i * 4 - j, 33 + i * 4 + j, angle, true, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            butterfly(t, 4 + i, 7 - i, 56 - 32 * i as i32, false, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hadamard(t, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                let angle = 24 + ((j as i32) << 6) + ((1 - i as i32) << 5);
                butterfly(t, 30 - 4 * i - j, 17 + 4 * i + j, angle, true, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hadamard(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
    for i in 0..2 {
        butterfly(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0, r);
    }
    if n >= 3 {
        for i in 0..2 {
            hadamard(t, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 14 - i, 9 + i, 48 + 64 * i as i32, true, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hadamard(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                let angle = 56 - (i as i32) * 32 + ((j as i32) >> 1) * 64;
                butterfly(t, 61 - i * 8 - j, 34 + i * 8 + j, angle, true, r);
            }
        }
    }
    for i in 0..2 {
        hadamard(t, i, 3 - i, false, r);
    }
    if n >= 3 {
        butterfly(t, 6, 5, 32, true, r);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hadamard(t, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 29 - i, 18 + i, 48 + ((i as i32) >> 1) * 64, true, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hadamard(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hadamard(t, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            butterfly(t, 13 - i, 10 + i, 32, true, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hadamard(t, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true, r);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hadamard(t, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            butterfly(t, 27 - i, 20 + i, 32, true, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hadamard(t, 32 + i, 47 - i, false, r);
            hadamard(t, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hadamard(t, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            butterfly(t, 55 - i, 40 + i, 32, true, r);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hadamard(t, i, 63 - i, false, r);
        }
    }
}

/// `Transform_Row_Shift` (spec 7.13.3) for the square transforms.
fn row_shift(log2: u32) -> usize {
    match log2 {
        2 => 0,
        3 => 1,
        _ => 2,
    }
}

/// The 2D inverse transform (spec 7.13.3) for a square `DCT_DCT` block.
///
/// `dequant` is the dequantized coefficient grid in raster order; the returned
/// residual is in the same order. A 64-point transform only ever carries
/// coefficients in its first 32 rows and columns, and the spec zeroes the rest
/// before the row transform, which is what the `< 32` guard does.
pub fn inverse_transform_2d(dequant: &[i32], side: usize, bit_depth: u8) -> Vec<i32> {
    let log2 = side.trailing_zeros();
    assert_eq!(1usize << log2, side, "the transform is square");
    assert_eq!(dequant.len(), side * side, "one coefficient per position");
    let row_clamp = usize::from(bit_depth) + 8;
    let col_clamp = (usize::from(bit_depth) + 6).max(16);
    let mut residual = vec![0i32; side * side];

    let mut t = [0i32; 64];
    for i in 0..side {
        for j in 0..side {
            t[j] = if i < 32 && j < 32 {
                dequant[i * side + j]
            } else {
                0
            };
        }
        inverse_dct(&mut t, log2, row_clamp);
        for j in 0..side {
            // The row's output is shifted, then clamped before the column
            // transform reads it back.
            residual[i * side + j] = clamp_range(round2(t[j], row_shift(log2)), col_clamp);
        }
    }

    for j in 0..side {
        for i in 0..side {
            t[i] = residual[i * side + j];
        }
        inverse_dct(&mut t, log2, col_clamp);
        for i in 0..side {
            residual[i * side + j] = round2(t[i], 4);
        }
    }
    residual
}

/// Dequantize (spec 7.12.3) and inverse transform one square `DCT_DCT` block,
/// returning its residual in raster order.
///
/// This is the encoder's model of what the decoder will add to its prediction,
/// so it follows the spec's dequantization exactly, truncation toward zero and
/// all.
pub fn dequant_and_inverse(levels: &[i32], side: usize, bit_depth: u8, q_idx: i32) -> Vec<i32> {
    let dq = crate::quant::dequant(levels, side, bit_depth, q_idx);
    inverse_transform_2d(&dq, side, bit_depth)
}

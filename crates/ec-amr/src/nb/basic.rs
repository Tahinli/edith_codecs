//! AMR-NB basic operations (saturating 16/32-bit fixed point), bit-exact with the
//! opencore-amr C-equivalent basic_op. Word16 = i16, Word32 = i32.
//! Plain functions have no overflow flag; `*_o` variants additionally set `*ovf = true`
//! when the result saturated. `round_` is C `round`/`pv_round`.
#![allow(dead_code)]

pub const MAX_16: i16 = 0x7fff;
pub const MIN_16: i16 = -0x8000;
pub const MAX_32: i32 = 0x7fff_ffff;
pub const MIN_32: i32 = i32::MIN;

use super::tables::{INV_SQRT_TBL, LOG2_TBL, POW2_TBL, SQRT_L_TBL};

// ---------------------------------------------------------------- 16-bit

/// C add (add_16). Saturating.
#[inline]
pub fn add_o(a: i16, b: i16, ovf: &mut bool) -> i16 {
    let s = a as i32 + b as i32;
    if s > 0x7fff {
        *ovf = true;
        MAX_16
    } else if s < -32768 {
        *ovf = true;
        MIN_16
    } else {
        s as i16
    }
}
#[inline]
pub fn add(a: i16, b: i16) -> i16 {
    (a as i32 + b as i32).clamp(-32768, 32767) as i16
}
/// C sub. Saturating.
#[inline]
pub fn sub_o(a: i16, b: i16, ovf: &mut bool) -> i16 {
    let s = a as i32 - b as i32;
    if s > 0x7fff {
        *ovf = true;
        MAX_16
    } else if s < -32768 {
        *ovf = true;
        MIN_16
    } else {
        s as i16
    }
}
#[inline]
pub fn sub(a: i16, b: i16) -> i16 {
    (a as i32 - b as i32).clamp(-32768, 32767) as i16
}
/// abs_s(-32768) = 32767.
#[inline]
pub fn abs_s(a: i16) -> i16 {
    if a == MIN_16 {
        MAX_16
    } else {
        a.abs()
    }
}
/// negate(-32768) = 32767.
#[inline]
pub fn negate(a: i16) -> i16 {
    if a == MIN_16 {
        MAX_16
    } else {
        -a
    }
}
/// (a*b)>>15, clipped to 0x7fff (only upper clip, as in C `mult`).
#[inline]
pub fn mult(a: i16, b: i16) -> i16 {
    let p = (a as i32 * b as i32) >> 15;
    if p > 0x7fff {
        MAX_16
    } else {
        p as i16
    }
}
/// (a*b + 0x4000)>>15 saturated.
#[inline]
pub fn mult_r_o(a: i16, b: i16, ovf: &mut bool) -> i16 {
    let p = (a as i32 * b as i32 + 0x4000) >> 15;
    if p > 0x7fff {
        *ovf = true;
        MAX_16
    } else if p < -32768 {
        *ovf = true;
        MIN_16
    } else {
        p as i16
    }
}
#[inline]
pub fn mult_r(a: i16, b: i16) -> i16 {
    mult_r_o(a, b, &mut false)
}

/// C shl (left shift with saturation; negative count shifts right, count <= -15 gives 0).
#[inline]
pub fn shl_o(var1: i16, var2: i16, ovf: &mut bool) -> i16 {
    if var2 < 0 {
        let n = -(var2 as i32);
        if n < 15 {
            var1 >> n
        } else {
            0
        }
    } else {
        let n = var2 as u32;
        let out: i16 = if n >= 32 { 0 } else { ((var1 as i32) << n) as i16 };
        let back = if n >= 32 { 0 } else { (out as i32) >> n };
        // C: (var_out >> var2) != var1 (var2 >= 16 can only match for var1 == 0)
        let chk = if n >= 16 { if out < 0 { -1 } else { 0 } } else { back };
        if chk != var1 as i32 {
            *ovf = true;
            (var1 >> 15) ^ MAX_16
        } else {
            out
        }
    }
}
#[inline]
pub fn shl(var1: i16, var2: i16) -> i16 {
    shl_o(var1, var2, &mut false)
}
/// C shr (arithmetic right shift; negative count shifts left with saturation).
#[inline]
pub fn shr_o(var1: i16, var2: i16, ovf: &mut bool) -> i16 {
    if var2 == 0 {
        var1
    } else if var2 > 0 {
        var1 >> (var2.min(15) as u32)
    } else {
        let n = ((-(var2 as i32)).min(15)) as u32;
        let result = ((var1 as i32) << n) as i16;
        if ((result as i32) >> n) != var1 as i32 {
            *ovf = true;
            if var1 > 0 { MAX_16 } else { MIN_16 }
        } else {
            result
        }
    }
}
#[inline]
pub fn shr(var1: i16, var2: i16) -> i16 {
    shr_o(var1, var2, &mut false)
}
/// shr with rounding.
#[inline]
pub fn shr_r(var1: i16, var2: i16) -> i16 {
    if var2 > 15 {
        0
    } else {
        let mut out = shr(var1, var2);
        if var2 > 0 && (var1 as i32 & (1 << (var2 - 1))) != 0 {
            out = out.wrapping_add(1);
        }
        out
    }
}
#[inline]
pub fn shr_r_o(var1: i16, var2: i16, ovf: &mut bool) -> i16 {
    if var2 > 15 {
        0
    } else {
        let mut out = shr_o(var1, var2, ovf);
        if var2 > 0 && (var1 as i32 & (1 << (var2 - 1))) != 0 {
            out = out.wrapping_add(1);
        }
        out
    }
}

// ---------------------------------------------------------------- 32-bit

#[inline]
pub fn l_add_o(a: i32, b: i32, ovf: &mut bool) -> i32 {
    match a.checked_add(b) {
        Some(v) => v,
        None => {
            *ovf = true;
            if a < 0 { MIN_32 } else { MAX_32 }
        }
    }
}
#[inline]
pub fn l_add(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}
#[inline]
pub fn l_sub_o(a: i32, b: i32, ovf: &mut bool) -> i32 {
    match a.checked_sub(b) {
        Some(v) => v,
        None => {
            *ovf = true;
            if a < 0 { MIN_32 } else { MAX_32 }
        }
    }
}
#[inline]
pub fn l_sub(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}
/// (a*b)<<1, saturating (only 0x8000*0x8000 overflows).
#[inline]
pub fn l_mult(a: i16, b: i16) -> i32 {
    let p = a as i32 * b as i32;
    if p != 0x4000_0000 {
        p << 1
    } else {
        MAX_32
    }
}
/// Plain 16x16 product, no doubling or saturation.
#[inline]
pub fn l_mult0(a: i16, b: i16) -> i32 {
    (a as i32).wrapping_mul(b as i32)
}
/// C L_mac: opencore semantics (0x4000_0000 product gives MAX_32).
#[inline]
pub fn l_mac_o(acc: i32, a: i16, b: i16, ovf: &mut bool) -> i32 {
    let p = a as i32 * b as i32;
    if p != 0x4000_0000 {
        l_add_o(acc, p << 1, ovf)
    } else {
        *ovf = true;
        MAX_32
    }
}
#[inline]
pub fn l_mac(acc: i32, a: i16, b: i16) -> i32 {
    l_mac_o(acc, a, b, &mut false)
}
/// C L_msu = L_sub(acc, L_mult(a,b)).
#[inline]
pub fn l_msu_o(acc: i32, a: i16, b: i16, ovf: &mut bool) -> i32 {
    l_sub_o(acc, l_mult(a, b), ovf)
}
#[inline]
pub fn l_msu(acc: i32, a: i16, b: i16) -> i32 {
    l_sub(acc, l_mult(a, b))
}
#[inline]
pub fn l_negate(a: i32) -> i32 {
    if a == MIN_32 { MAX_32 } else { -a }
}
#[inline]
pub fn l_abs(a: i32) -> i32 {
    if a == MIN_32 { MAX_32 } else { a.abs() }
}
#[inline]
pub fn l_deposit_h(a: i16) -> i32 {
    (a as i32) << 16
}
#[inline]
pub fn l_deposit_l(a: i16) -> i32 {
    a as i32
}
#[inline]
pub fn extract_h(a: i32) -> i16 {
    (a >> 16) as i16
}
#[inline]
pub fn extract_l(a: i32) -> i16 {
    a as i16
}
/// C L_shl: left shift with saturation (negative count: arithmetic right, count <= -31 gives 0).
#[inline]
pub fn l_shl_o(x: i32, n: i16, ovf: &mut bool) -> i32 {
    if n > 0 {
        let n = n as u32;
        let ok = if n >= 32 { x == 0 } else { ((x << n) >> n) == x };
        if ok {
            if n >= 32 { 0 } else { x << n }
        } else {
            *ovf = true;
            (x >> 31) ^ MAX_32
        }
    } else {
        let m = -(n as i32);
        if m < 31 { x >> m } else { 0 }
    }
}
#[inline]
pub fn l_shl(x: i32, n: i16) -> i32 {
    l_shl_o(x, n, &mut false)
}
/// C L_shr: arithmetic right shift (count >= 31 gives 0, negative count shifts left saturating).
#[inline]
pub fn l_shr_o(x: i32, n: i16, ovf: &mut bool) -> i32 {
    if n > 0 {
        if n < 31 { x >> n } else { 0 }
    } else {
        let m = -(n as i32);
        let ok = if m >= 32 { x == 0 } else { ((x << m) >> m) == x };
        if ok {
            if m >= 32 { 0 } else { x << m }
        } else {
            *ovf = true;
            (x >> 31) ^ MAX_32
        }
    }
}
#[inline]
pub fn l_shr(x: i32, n: i16) -> i32 {
    l_shr_o(x, n, &mut false)
}
/// L_shr with rounding.
#[inline]
pub fn l_shr_r(x: i32, n: i16) -> i32 {
    if n > 31 {
        0
    } else {
        let mut r = l_shr(x, n);
        if n > 0 && (x & (1i32 << (n - 1))) != 0 {
            r = r.wrapping_add(1);
        }
        r
    }
}
/// C round / pv_round: round 32 to high 16 bits.
#[inline]
pub fn round_(x: i32) -> i16 {
    (l_add(x, 0x8000) >> 16) as i16
}
/// L_mac then round.
#[inline]
pub fn mac_r(acc: i32, a: i16, b: i16) -> i16 {
    round_(l_mac(acc, a, b))
}
/// L_msu then round.
#[inline]
pub fn msu_r(acc: i32, a: i16, b: i16) -> i16 {
    round_(l_msu(acc, a, b))
}

/// Normalisation shift count for 16-bit (0 for 0 input).
#[inline]
pub fn norm_s(x: i16) -> i16 {
    if x == 0 {
        0
    } else {
        // opencore: y = x - (x<0); v = y ^ (y>>15)  (this equals |x|, not ~x)
        let y = x.wrapping_sub((x < 0) as i16);
        let v = y ^ (y >> 15);
        (v.leading_zeros() as i16) - 1
    }
}
/// Normalisation shift count for 32-bit (0 for 0 input).
#[inline]
pub fn norm_l(x: i32) -> i16 {
    if x == 0 {
        0
    } else {
        let y = x.wrapping_sub((x < 0) as i32);
        let v = y ^ (y >> 31);
        (v.leading_zeros() as i16) - 1
    }
}

/// C div_s: Q15 fraction var1/var2; requires 0 <= var1 <= var2, else 0.
#[inline]
pub fn div_s(var1: i16, var2: i16) -> i16 {
    if var1 > var2 || var1 < 0 {
        return 0;
    }
    if var1 == 0 {
        return 0;
    }
    if var1 == var2 {
        return MAX_16;
    }
    let mut out: i16 = 0;
    let mut num = var1 as i32;
    let den = var2 as i32;
    for _ in 0..5 {
        out <<= 3;
        num <<= 3;
        if num >= den << 2 {
            num -= den << 2;
            out |= 4;
        }
        if num >= den << 1 {
            num -= den << 1;
            out |= 2;
        }
        if num >= den {
            num -= den;
            out |= 1;
        }
    }
    out
}

// ---------------------------------------------------------------- 32-bit via hi/lo

/// C L_Extract: split into (hi, lo) with lo in 0..0x7fff.
#[inline]
pub fn l_extract(x: i32, hi: &mut i16, lo: &mut i16) {
    let h = (x >> 16) as i16;
    *hi = h;
    *lo = ((x >> 1) - ((h as i32) << 15)) as i16;
}
/// C L_Comp: inverse of l_extract.
#[inline]
pub fn l_comp(hi: i16, lo: i16) -> i32 {
    l_mac(l_deposit_h(hi), lo, 1)
}
/// C Mpy_32: (hi1/lo1) * (hi2/lo2), result in Q31 (saturating as in C).
#[inline]
pub fn mpy_32(h1: i16, l1: i16, h2: i16, l2: i16) -> i32 {
    let mut p = h1 as i32 * h2 as i32;
    p = if p != 0x4000_0000 { p << 1 } else { MAX_32 };
    let p32 = (h1 as i32 * l2 as i32) >> 15;
    let s = p.wrapping_add(p32 << 1);
    let mut sum = s;
    if (p ^ p32) > 0 && ((s ^ p) >> 31) != 0 {
        sum = if (p >> 31) != 0 { MIN_32 } else { MAX_32 };
    }
    let p = sum;
    let p32 = (l1 as i32 * h2 as i32) >> 15;
    let s = p.wrapping_add(p32 << 1);
    if (p ^ p32) > 0 && ((s ^ p) >> 31) != 0 {
        return if (p >> 31) != 0 { MIN_32 } else { MAX_32 };
    }
    s
}
/// C Mpy_32_16: (hi/lo) * var2.
#[inline]
pub fn mpy_32_16(hi: i16, lo: i16, v: i16) -> i32 {
    let mut p = hi as i32 * v as i32;
    p = if p != 0x4000_0000 { p << 1 } else { MAX_32 };
    let r = (lo as i32 * v as i32) >> 15;
    let s = p.wrapping_add(r << 1);
    if (p ^ r) > 0 && ((s ^ p) >> 31) != 0 {
        return if (p >> 31) != 0 { MIN_32 } else { MAX_32 };
    }
    s
}
/// C Mac_32: acc + (h1/l1)*(h2/l2).
#[inline]
pub fn mac_32(acc: i32, h1: i16, l1: i16, h2: i16, l2: i16) -> i32 {
    let mut a = l_mac(acc, h1, h2);
    a = l_mac(a, mult(h1, l2), 1);
    l_mac(a, mult(l1, h2), 1)
}
/// C Mac_32_16: acc + (hi/lo)*var2.
#[inline]
pub fn mac_32_16(acc: i32, hi: i16, lo: i16, v: i16) -> i32 {
    let a = l_mac(acc, hi, v);
    l_mac(a, mult(lo, v), 1)
}
/// C Div_32: L_num / (denom_hi/denom_lo), result Q31.
#[inline]
pub fn div_32(l_num: i32, d_hi: i16, d_lo: i16) -> i32 {
    let approx = div_s(0x3fff, d_hi);
    let mut r = mpy_32_16(d_hi, d_lo, approx);
    r = 0x7fff_ffff - r;
    let hi = (r >> 16) as i16;
    let lo = ((r >> 1) - ((hi as i32) << 15)) as i16;
    r = mpy_32_16(hi, lo, approx);
    let hi = (r >> 16) as i16;
    let lo = ((r >> 1) - ((hi as i32) << 15)) as i16;
    let n_hi = (l_num >> 16) as i16;
    let n_lo = ((l_num >> 1) - ((n_hi as i32) << 15)) as i16;
    r = mpy_32(n_hi, n_lo, hi, lo);
    l_shl(r, 2)
}

// ---------------------------------------------------------------- math functions

/// C Pow2: 2^(exponent + fraction/32768) as Q0 32-bit (exponent 0..30, fraction Q15).
#[inline]
pub fn pow2(exponent: i16, fraction: i16) -> i32 {
    let lx = l_mult(fraction, 32);
    let i = (((lx >> 16) as i16) & 31) as usize;
    let a = ((lx >> 1) & 0x7fff) as i16;
    let mut x = (POW2_TBL[i] as i32) << 16;
    let tmp = POW2_TBL[i].wrapping_sub(POW2_TBL[i + 1]);
    x = l_msu(x, tmp, a);
    let exp = 30i16.wrapping_sub(exponent);
    l_shr_r(x, exp)
}
/// C Log2_norm: x already normalised by `exp` shifts.
#[inline]
pub fn log2_norm(x: i32, exp: i16, exponent: &mut i16, fraction: &mut i16) {
    if x <= 0 {
        *exponent = 0;
        *fraction = 0;
    } else {
        *exponent = 30i16.wrapping_sub(exp);
        let x = x >> 10;
        let i = ((x >> 15) as i16).wrapping_sub(32) as usize;
        let a = (x & 0x7fff) as i32;
        let mut y = (LOG2_TBL[i] as i32) << 16;
        let tmp = LOG2_TBL[i].wrapping_sub(LOG2_TBL[i + 1]) as i32;
        y = y.wrapping_sub((tmp * a) << 1);
        *fraction = (y >> 16) as i16;
    }
}
/// C Log2: log2(l_x) = exponent + fraction (Q15).
#[inline]
pub fn log2(l_x: i32, exponent: &mut i16, fraction: &mut i16) {
    let exp = norm_l(l_x);
    let r = l_x.wrapping_shl(exp as u32);
    log2_norm(r, exp, exponent, fraction);
}
/// C Inv_sqrt: 1/sqrt(x), Q30 result (x Q0).
#[inline]
pub fn inv_sqrt(l_x: i32) -> i32 {
    if l_x <= 0 {
        return 0x3fff_ffff;
    }
    let mut exp = norm_l(l_x);
    let mut x = l_x << exp;
    exp = 30 - exp;
    if (exp & 1) == 0 {
        x >>= 1;
    }
    exp >>= 1;
    exp += 1;
    x >>= 9;
    let i = ((x >> 16) as i16 - 16) as usize;
    let a = ((x >> 1) as i16 & 0x7fff) as i32;
    let mut y = (INV_SQRT_TBL[i] as i32) << 16;
    let tmp = (INV_SQRT_TBL[i] - INV_SQRT_TBL[i + 1]) as i32;
    y = y.wrapping_sub((tmp * a) << 1);
    y >> exp
}
/// C sqrt_l_exp: returns sqrt mantissa, exponent in *exp.
#[inline]
pub fn sqrt_l_exp(l_x: i32, exp: &mut i16) -> i32 {
    if l_x <= 0 {
        *exp = 0;
        return 0;
    }
    let e = norm_l(l_x) & !1;
    let x = l_shl(l_x, e);
    *exp = e;
    let x = x >> 10;
    let mut i = ((x >> 15) & 63) as usize;
    let a = (x as i16 & 0x7fff) as i16;
    if i > 15 {
        i -= 16;
    }
    let y = (SQRT_L_TBL[i] as i32) << 16;
    let tmp = SQRT_L_TBL[i].wrapping_sub(SQRT_L_TBL[i + 1]);
    l_msu(y, tmp, a)
}
/// C gmed_n: median of the first n (<= 9) values of ind (n odd as used by the codec).
#[inline]
pub fn gmed_n(ind: &[i16], n: usize) -> i16 {
    let mut tmp2 = [0i16; 9];
    let mut tmp = [0usize; 9];
    tmp2[..n].copy_from_slice(&ind[..n]);
    let mut ix = 0usize;
    for i in 0..n {
        let mut max = -32767i16;
        for j in 0..n {
            if tmp2[j] >= max {
                max = tmp2[j];
                ix = j;
            }
        }
        tmp2[ix] = -32768;
        tmp[i] = ix;
    }
    ind[tmp[n >> 1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturation() {
        assert_eq!(add(32767, 1), 32767);
        assert_eq!(sub(-32768, 1), -32768);
        assert_eq!(abs_s(-32768), 32767);
        assert_eq!(negate(-32768), 32767);
        assert_eq!(l_add(i32::MAX, 1), i32::MAX);
        assert_eq!(l_sub(i32::MIN, 1), i32::MIN);
        assert_eq!(l_mult(-32768, -32768), i32::MAX);
        assert_eq!(l_mac(5, -32768, -32768), i32::MAX);
        assert_eq!(mult(-32768, -32768), 32767);
        assert_eq!(mult_r(-32768, -32768), 32767);
        assert_eq!(shl(0x4000, 1), 32767);
        assert_eq!(shl(-0x4001, 1), -32768);
        assert_eq!(shl(3, 2), 12);
        assert_eq!(shl(-8, -2), -2);
        assert_eq!(shr(1, -15), 32767);
        assert_eq!(shr(-5, 1), -3);
        assert_eq!(shr_r(5, 1), 3);
        assert_eq!(l_shl(0x4000_0000, 1), i32::MAX);
        assert_eq!(l_shl(-0x4000_0001, 1), i32::MIN);
        assert_eq!(l_shr_r(5, 1), 3);
        assert_eq!(l_shr(1, -31), i32::MAX);
        let mut o = false;
        add_o(32767, 1, &mut o);
        assert!(o);
    }

    #[test]
    fn norms_and_math() {
        assert_eq!(norm_s(0), 0);
        assert_eq!(norm_s(1), 14);
        assert_eq!(norm_s(-1), 14);
        assert_eq!(norm_s(-32768), 0);
        assert_eq!(norm_l(1), 30);
        assert_eq!(norm_l(-1), 30);
        assert_eq!(norm_l(i32::MIN), 0);
        assert_eq!(div_s(1, 2), 16384);
        assert_eq!(div_s(5, 5), 32767);
        assert_eq!(round_(0x7fff_ffff), 32767);
        assert_eq!(pow2(14, 0), 16384);
        assert_eq!(inv_sqrt(0), 0x3fff_ffff);
        let (mut e, mut f) = (0, 0);
        log2(1 << 20, &mut e, &mut f);
        assert_eq!((e, f), (20, 0));
        assert_eq!(gmed_n(&[5, 1, 9, 3, 7], 5), 5);
        let (mut h, mut l) = (0, 0);
        l_extract(0x1234_5678, &mut h, &mut l);
        assert_eq!(l_comp(h, l), 0x1234_5678);
    }
}


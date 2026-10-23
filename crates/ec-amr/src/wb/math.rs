//! Port of pvamrwb_math_op.cpp (opencore AMR-WB decoder math helpers).
use super::ops::*;

#[inline]
pub fn extract_h(x: i32) -> i16 {
    (x >> 16) as i16
}
#[inline]
pub fn extract_l(x: i32) -> i16 {
    x as i16
}
#[inline]
pub fn l_deposit_h(x: i16) -> i32 {
    (x as i32).wrapping_shl(16)
}
#[inline]
pub fn l_deposit_l(x: i16) -> i32 {
    x as i32
}

/// Ports `mult_int16_r`.
pub fn mult_int16_r(var1: i16, var2: i16) -> i16 {
    let mut p: i32 = (var1 as i32).wrapping_mul(var2 as i32);
    p = p.wrapping_add(0x4000);
    p >>= 15;
    if (p >> 15) != (p >> 31) {
        p = (p >> 31) ^ (MAX_16 as i32);
    }
    p as i16
}

/// Ports `shr_rnd`.
pub fn shr_rnd(var1: i16, var2: i16) -> i16 {
    let mut out: i16 = (var1 as i32 >> ((var2 & 0xf) as u32)) as i16;
    if var2 != 0 {
        let mask = (1i32.wrapping_shl(((var2 as i32) - 1) as u32 & 31)) as i16;
        if (var1 & mask) != 0 {
            out = out.wrapping_add(1);
        }
    }
    out
}

/// Ports `div_16by16`.
pub fn div_16by16(var1: i16, var2: i16) -> i16 {
    let mut var_out: i16 = 0;
    if var1 > var2 || var1 < 0 {
        return 0;
    }
    if var1 != 0 {
        if var1 != var2 {
            let mut l_num = var1 as i32;
            let l_denom = var2 as i32;
            let l_denom_by_2 = l_denom << 1;
            let l_denom_by_4 = l_denom << 2;
            for _ in 0..5 {
                var_out = var_out.wrapping_shl(3);
                l_num = l_num.wrapping_shl(3);
                if l_num >= l_denom_by_4 {
                    l_num -= l_denom_by_4;
                    var_out |= 4;
                }
                if l_num >= l_denom_by_2 {
                    l_num -= l_denom_by_2;
                    var_out |= 2;
                }
                if l_num >= l_denom {
                    l_num -= l_denom;
                    var_out |= 1;
                }
            }
        } else {
            var_out = MAX_16;
        }
    }
    var_out
}

/// Ports `one_ov_sqrt`.
pub fn one_ov_sqrt(l_x: i32) -> i32 {
    let mut exp = normalize_amr_wb(l_x);
    let mut l_x = l_x.wrapping_shl(exp as u32);
    exp = 31 - exp;
    one_ov_sqrt_norm(&mut l_x, &mut exp);
    shl_int32(l_x, exp)
}

pub static table_isqrt: [i16; 49] = [
    32767, 31790, 30894, 30070, 29309, 28602, 27945, 27330, 26755, 26214, 25705, 25225, 24770,
    24339, 23930, 23541, 23170, 22817, 22479, 22155, 21845, 21548, 21263, 20988, 20724, 20470,
    20225, 19988, 19760, 19539, 19326, 19119, 18919, 18725, 18536, 18354, 18176, 18004, 17837,
    17674, 17515, 17361, 17211, 17064, 16921, 16782, 16646, 16514, 16384,
];

/// Ports `one_ov_sqrt_norm`.
pub fn one_ov_sqrt_norm(frac: &mut i32, exp: &mut i16) {
    if *frac <= 0 {
        *exp = 0;
        *frac = 0x7fff_ffff;
        return;
    }
    if (*exp & 1) == 1 {
        *frac >>= 1;
    }
    *exp = negate_int16(((*exp as i32 - 1) >> 1) as i16);
    *frac >>= 9;
    let mut i = extract_h(*frac);
    *frac >>= 1;
    let mut a = *frac as i16;
    a &= 0x7fff;
    i = i.wrapping_sub(16);
    let iu = i as usize;
    *frac = l_deposit_h(table_isqrt[iu]);
    let tmp = table_isqrt[iu].wrapping_sub(table_isqrt[iu + 1]);
    *frac = msu_16by16_from_int32(*frac, tmp, a);
}

pub static table_pow2: [i16; 33] = [
    16384, 16743, 17109, 17484, 17867, 18258, 18658, 19066, 19484, 19911, 20347, 20792, 21247,
    21713, 22188, 22674, 23170, 23678, 24196, 24726, 25268, 25821, 26386, 26964, 27554, 28158,
    28774, 29405, 30048, 30706, 31379, 32066, 32767,
];

/// Ports `power_of_2`.
pub fn power_of_2(exponant: i16, fraction: i16) -> i32 {
    let mut l_x: i32 = (fraction as i32) << 5;
    let i = (fraction >> 10) as usize;
    let mut a = l_x as i16;
    a &= 0x7fff;
    l_x = (table_pow2[i] as i32) << 15;
    let tmp = table_pow2[i].wrapping_sub(table_pow2[i + 1]);
    l_x = l_x.wrapping_sub((tmp as i32).wrapping_mul(a as i32));
    let exp = (29i32 - exponant as i32) as i16;
    if exp != 0 {
        l_x = (l_x >> (exp as u32 & 31)).wrapping_add((l_x >> ((exp as i32 - 1) as u32 & 31)) & 1);
    }
    l_x
}

/// Ports `Dot_product12`. Returns (result, exp).
pub fn dot_product12(x: &[i16], y: &[i16], lg: i16, exp: &mut i16) -> i32 {
    let mut l_sum: i32 = 1;
    let n = ((lg >> 3) as usize) * 8;
    for k in 0..n {
        l_sum = mac_16by16_to_int32(l_sum, x[k], y[k]);
    }
    let sft = normalize_amr_wb(l_sum);
    l_sum = l_sum.wrapping_shl(sft as u32);
    *exp = 30 - sft;
    l_sum
}

pub static Log2_norm_table: [i16; 33] = [
    0, 1455, 2866, 4236, 5568, 6863, 8124, 9352, 10549, 11716, 12855, 13967, 15054, 16117, 17156,
    18172, 19167, 20142, 21097, 22033, 22951, 23852, 24735, 25603, 26455, 27291, 28113, 28922,
    29716, 30497, 31266, 32023, 32767,
];

/// Ports `Lg2_normalized`.
pub fn lg2_normalized(l_x: i32, exp: i16, exponent: &mut i16, fraction: &mut i16) {
    if l_x <= 0 {
        *exponent = 0;
        *fraction = 0;
        return;
    }
    *exponent = 30 - exp;
    let mut l_x = l_x >> 9;
    let mut i = extract_h(l_x);
    l_x >>= 1;
    let mut a = l_x as i16;
    a &= 0x7fff;
    i = i.wrapping_sub(32);
    let iu = i as usize;
    let mut l_y = l_deposit_h(Log2_norm_table[iu]);
    let tmp = Log2_norm_table[iu].wrapping_sub(Log2_norm_table[iu + 1]);
    l_y = msu_16by16_from_int32(l_y, tmp, a);
    *fraction = extract_h(l_y);
}

/// Ports `amrwb_log_2`.
pub fn amrwb_log_2(l_x: i32, exponent: &mut i16, fraction: &mut i16) {
    let exp = normalize_amr_wb(l_x);
    lg2_normalized(shl_int32(l_x, exp), exp, exponent, fraction);
}

/// Ports `int32_to_dpf`. Returns (hi, lo).
pub fn int32_to_dpf(l_32: i32) -> (i16, i16) {
    let hi = (l_32 >> 16) as i16;
    let lo = (l_32.wrapping_sub((hi as i32).wrapping_shl(16)) >> 1) as i16;
    (hi, lo)
}

/// Ports `mpy_dpf_32`.
pub fn mpy_dpf_32(hi1: i16, lo1: i16, hi2: i16, lo2: i16) -> i32 {
    let mut l = mul_16by16_to_int32(hi1, hi2);
    l = mac_16by16_to_int32(l, mult_int16(hi1, lo2), 1);
    l = mac_16by16_to_int32(l, mult_int16(lo1, hi2), 1);
    l
}

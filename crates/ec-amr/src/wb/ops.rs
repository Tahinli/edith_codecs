//! ETSI-style saturating basic operators (port of the 3GPP TS 26.173 basic_op set).
#![allow(dead_code)]

pub const MAX_16: i16 = 32767;
pub const MIN_16: i16 = -32768;
pub const MAX_32: i32 = 0x7fff_ffff;
pub const MIN_32: i32 = i32::MIN;

#[inline]
pub fn sat16(x: i32) -> i16 {
    x.clamp(-32768, 32767) as i16
}

#[inline]
pub fn add_int16(a: i16, b: i16) -> i16 {
    sat16(a as i32 + b as i32)
}
#[inline]
pub fn sub_int16(a: i16, b: i16) -> i16 {
    sat16(a as i32 - b as i32)
}
#[inline]
pub fn mult_int16(a: i16, b: i16) -> i16 {
    sat16((a as i32 * b as i32) >> 15)
}
#[inline]
pub fn negate_int16(a: i16) -> i16 {
    if a == MIN_16 { MAX_16 } else { -a }
}
#[inline]
pub fn add_int32(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}
#[inline]
pub fn sub_int32(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}
/// L_mult: (a*b)<<1 with saturation of the single overflow case.
#[inline]
pub fn mul_16by16_to_int32(a: i16, b: i16) -> i32 {
    let m = a as i32 * b as i32;
    if m != 0x4000_0000 { m << 1 } else { MAX_32 }
}
/// L_mac
#[inline]
pub fn mac_16by16_to_int32(acc: i32, a: i16, b: i16) -> i32 {
    acc.saturating_add(mul_16by16_to_int32(a, b))
}
/// L_msu
#[inline]
pub fn msu_16by16_from_int32(acc: i32, a: i16, b: i16) -> i32 {
    acc.saturating_sub(mul_16by16_to_int32(a, b))
}
/// round(): extract_h(L + 0x8000) with the C code's special-case for MAX_32.
#[inline]
pub fn amr_wb_round(l: i32) -> i16 {
    let l = if l != MAX_32 {
        l.wrapping_add(0x8000)
    } else {
        l
    };
    (l >> 16) as i16
}
#[inline]
pub fn amr_wb_shl1_round(l: i32) -> i16 {
    if (l.wrapping_shl(1)) >> 1 == l {
        ((l.wrapping_add(0x4000)) >> 15) as i16
    } else {
        (((l >> 31) ^ MAX_32) >> 16) as i16
    }
}
#[inline]
pub fn mul_32by16(hi: i16, lo: i16, n: i16) -> i32 {
    (((hi as i32 * n as i32) + ((lo as i32 * n as i32) >> 15)) as i32).wrapping_shl(1)
}
/// plain (non saturating) 16x16 -> 32 multiply-accumulate
#[inline]
pub fn fxp_mac_16by16(a: i16, b: i16, acc: i32) -> i32 {
    acc.wrapping_add(a as i32 * b as i32)
}
#[inline]
pub fn fxp_mul_16by16(a: i16, b: i16) -> i32 {
    a as i32 * b as i32
}
/// (l1 * (l2<<16)) >> 32 ; l2 is a 16-bit value passed as i32 (C passes int32 and shifts it).
#[inline]
pub fn fxp_mul32_by_16b(l1: i32, l2: i32) -> i32 {
    (((l1 as i64) * ((l2.wrapping_shl(16)) as i64)) >> 32) as i32
}
/// shl for 16-bit, negative shift = arithmetic right (count masked & 0xf as in the C code).
#[inline]
pub fn shl_int16(v: i16, s: i16) -> i16 {
    if s < 0 {
        let s = ((-(s as i32)) & 0xf) as u32;
        v >> s
    } else {
        let s = (s & 0xf) as u32;
        let out = ((v as i32) << s) as i16;
        if ((out as i32) >> s) != v as i32 {
            ((v >> 15) ^ MAX_16) as i16
        } else {
            out
        }
    }
}
#[inline]
pub fn shl_int32(v: i32, s: i16) -> i32 {
    if s > 0 {
        let s = (s as u32) & 31;
        let out = v.wrapping_shl(s);
        if (out >> s) != v {
            (v >> 31) ^ MAX_32
        } else {
            out
        }
    } else {
        let s = ((-(s as i32)) & 0xf) as u32;
        v >> s
    }
}
#[inline]
pub fn shr_int32(v: i32, s: i16) -> i32 {
    if s >= 0 {
        v >> ((s as u32) & 0x1f)
    } else {
        let s = ((-(s as i32)) as u32) & 0x1f;
        let out = v.wrapping_shl(s);
        if (out >> s) != v {
            (v >> 31) ^ MAX_32
        } else {
            out
        }
    }
}

/// normalize_amr_wb (C-equivalent, == norm_l for positive input).
pub fn normalize_amr_wb(x: i32) -> i16 {
    let mut i: i32 = if x > 0x0FFF_FFFF {
        0
    } else if x > 0x00FF_FFFF {
        3
    } else if x > 0x0000_FFFF {
        if x > 0x000F_FFFF { 7 } else { 11 }
    } else if x > 0x0000_00FF {
        if x > 0x0000_0FFF { 15 } else { 19 }
    } else if x > 0x0000_000F {
        23
    } else {
        27
    };
    let xs = x.wrapping_shl(i as u32);
    match xs & 0x7800_0000 {
        0x0800_0000 => i += 3,
        0x1800_0000 | 0x1000_0000 => i += 2,
        0x2800_0000 | 0x2000_0000 | 0x3800_0000 | 0x3000_0000 => i += 1,
        _ => {}
    }
    i as i16
}
#[inline]
pub fn norm_l(x: i32) -> i16 {
    normalize_amr_wb(x)
}
#[inline]
pub fn norm_s(x: i16) -> i16 {
    normalize_amr_wb(x as i32) - 16
}

// ---- codec constants (pvamrwbdecoder_cnst.h) ----
pub const L_FRAME: usize = 256;
pub const L_SUBFR16K: usize = 80;
pub const L_SUBFR: usize = 64;
pub const NB_SUBFR: usize = 4;
pub const M: usize = 16;
pub const M16K: usize = 20;
pub const L_FILT16K: usize = 15;
pub const L_FILT: usize = 12;
pub const GP_CLIP: i16 = 15565;
pub const PIT_SHARP: i16 = 27853;
pub const PIT_MIN: i16 = 34;
pub const PIT_FR2: i16 = 128;
pub const PIT_FR1_9B: i16 = 160;
pub const PIT_FR1_8B: i16 = 92;
pub const PIT_MAX: i16 = 231;
pub const L_INTERPOL: usize = 17;
pub const PREEMPH_FAC: i16 = 22282;
pub const GAMMA1: i16 = 30147;
pub const TILT_FAC: i16 = 22282;
pub const Q_MAX: i16 = 8;
pub const RANDOM_INITSEED: i16 = 21845;
pub const L_MEANBUF: usize = 3;
pub const ONE_PER_MEANBUF: i16 = 10923;
pub const EHF_MASK: i16 = 0x0008;
pub const AMR_WB_PCM_FRAME: usize = 320;
pub const NBBITS_7K: i16 = 132;
pub const NBBITS_9K: i16 = 177;
pub const NBBITS_12K: i16 = 253;
pub const NBBITS_14K: i16 = 285;
pub const NBBITS_16K: i16 = 317;
pub const NBBITS_18K: i16 = 365;
pub const NBBITS_20K: i16 = 397;
pub const NBBITS_23K: i16 = 461;
pub const NBBITS_24K: i16 = 477;
pub const NBBITS_SID: i16 = 35;
pub const AMR_WB_COMPRESSED: [i16; 10] = [132, 177, 253, 285, 317, 365, 397, 461, 477, 35];

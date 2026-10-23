//! AMR-NB excitation decoding: pitch lag, algebraic codebooks, gains, phase dispersion.
//! Bit-exact port of the 3GPP TS 26.073 reference (opencore-amr flavour).
#![allow(clippy::all, dead_code, unused_imports, unused_variables, unused_assignments)]

use super::basic::*;
use super::tables::*;

const L_SUBFR_: usize = 40;

#[inline]
fn m15(a: i32, b: i32) -> i32 {
    (a * b) >> 15
}

/// 6-phase interpolation filter of Pred_lt_3or6 (61 taps).
static INTER_6_PRED_LT: [i16; 61] = [29443,28346,25207,20449,14701,8693,3143,-1352,-4402,-5865,-5850,-4673,-2783,-672,1211,2536,3130,2991,2259,1170,0,-1001,-1652,-1868,-1666,-1147,-464,218,756,1060,1099,904,550,135,-245,-514,-634,-602,-451,-231,0,191,308,340,296,198,78,-36,-120,-163,-165,-132,-79,-19,34,73,91,89,70,38,0];

// ---------------------------------------------------------------- pitch lag

/// C `Dec_lag3`: decode 1/3 (or 1/6 if flag4==0 for i_subfr!=0 case per C) resolution lag.
/// Outputs `(T0, T0_frac)` through `t0`/`t0_frac`. `t0_prev` = previous subframe T0.
#[inline]
pub fn dec_lag3(
    index: i16,
    t0_min: i16,
    t0_max: i16,
    i_subfr: i16,
    t0_prev: i16,
    t0: &mut i16,
    t0_frac: &mut i16,
    flag4: i16,
) {
    let index = index as i32;
    let t0_min = t0_min as i32;
    let t0_max = t0_max as i32;
    if i_subfr == 0 {
        if index < 197 {
            let tmp_lag = m15(index + 2, 10923);
            let mut i = tmp_lag + 19;
            *t0 = i as i16;
            i <<= 1;
            i += *t0 as i32;
            *t0_frac = (index - i + 58) as i16;
        } else {
            *t0 = (index - 112) as i16;
            *t0_frac = 0;
        }
    } else if flag4 == 0 {
        let mut i = m15(index + 2, 10923);
        i -= 1;
        *t0 = (i + t0_min) as i16;
        i += i << 1;
        *t0_frac = (index - 2 - i) as i16;
    } else {
        let mut tmp_lag = t0_prev as i32;
        let i = sub(t0_prev, t0_min as i16) as i32;
        if i > 5 {
            tmp_lag = t0_min + 5;
        }
        let i = t0_max - tmp_lag;
        if i > 4 {
            tmp_lag = t0_max - 4;
        }
        if index < 4 {
            let i = tmp_lag - 5;
            *t0 = (i + index) as i16;
            *t0_frac = 0;
        } else if index < 12 {
            let mut i = m15(index - 5, 10923);
            i -= 1;
            *t0 = (i + tmp_lag) as i16;
            i += i << 1;
            *t0_frac = (index - 9 - i) as i16;
        } else {
            let i = index - 12 + tmp_lag;
            *t0 = (i + 1) as i16;
            *t0_frac = 0;
        }
    }
}

/// C `Dec_lag6`: decode 1/6 resolution lag. `t0` is in/out (previous T0 on entry when i_subfr!=0).
#[inline]
pub fn dec_lag6(
    index: i16,
    pit_min: i16,
    pit_max: i16,
    i_subfr: i16,
    t0: &mut i16,
    t0_frac: &mut i16,
) {
    let index = index as i32;
    if i_subfr == 0 {
        if index < 463 {
            let mut i = m15(index + 5, 5462);
            i += 17;
            *t0 = i as i16;
            i <<= 1;
            i += *t0 as i32;
            i <<= 1;
            *t0_frac = (index - i + 105) as i16;
        } else {
            *t0 = (index - 368) as i16;
            *t0_frac = 0;
        }
    } else {
        let mut t0_min = *t0 as i32 - 5;
        if t0_min < pit_min as i32 {
            t0_min = pit_min as i32;
        }
        let mut t0_max = t0_min + 9;
        if t0_max > pit_max as i32 {
            t0_max = pit_max as i32;
            t0_min = t0_max - 9;
        }
        let mut i = m15(index + 5, 5462);
        i -= 1;
        *t0 = (i + t0_min) as i16;
        i += i << 1;
        i <<= 1;
        let k = index - 3;
        *t0_frac = (k - i) as i16;
    }
}

/// C `Pred_lt_3or6`: adaptive codebook (long-term prediction) by interpolation.
/// `exc` is the excitation buffer; `off` is the index of C's `exc[0]` (start of the
/// current subframe), so past excitation is read at `exc[off - T0 - ..]` (need off >= T0 + 11).
/// Output (`l_subfr` samples) is written in place at `exc[off..off+l_subfr]`.
/// `flag3`: nonzero => 1/3 resolution (frac doubled), else 1/6.
#[inline]
pub fn pred_lt_3or6(exc: &mut [i16], off: usize, t0: i16, frac: i16, l_subfr: i16, flag3: i16) {
    const UP: i32 = 6;
    const L_INTER10: usize = 10;
    let mut frac = -(frac as i32);
    if flag3 != 0 {
        frac <<= 1;
    }
    let mut p0 = off as isize - t0 as isize;
    if frac < 0 {
        frac += UP;
        p0 -= 1;
    }
    let c1 = frac as usize;
    let c2 = (UP - frac) as usize;
    let mut coeff = [0i32; 2 * L_INTER10];
    let mut k = 0usize;
    let mut ci = 0usize;
    for _ in 0..(L_INTER10 >> 1) {
        coeff[ci] = INTER_6_PRED_LT[c1 + k] as i32;
        coeff[ci + 1] = INTER_6_PRED_LT[c2 + k] as i32;
        k += 6;
        coeff[ci + 2] = INTER_6_PRED_LT[c1 + k] as i32;
        coeff[ci + 3] = INTER_6_PRED_LT[c2 + k] as i32;
        k += 6;
        ci += 4;
    }
    let mut out = off;
    for _ in 0..(l_subfr >> 1) {
        p0 += 1;
        let mut p2 = p0;
        let mut p3 = p0;
        p0 += 1;
        let mut c = 0usize;
        let mut s1: i32 = 0x4000;
        let mut s2: i32 = 0x4000;
        for _ in 0..(L_INTER10 >> 1) {
            for _ in 0..2 {
                s2 = s2.wrapping_add((exc[p3 as usize] as i32).wrapping_mul(coeff[c]));
                p3 -= 1;
                s1 = s1.wrapping_add((exc[p3 as usize] as i32).wrapping_mul(coeff[c]));
                c += 1;
                s1 = s1.wrapping_add((exc[p2 as usize] as i32).wrapping_mul(coeff[c]));
                p2 += 1;
                s2 = s2.wrapping_add((exc[p2 as usize] as i32).wrapping_mul(coeff[c]));
                c += 1;
            }
        }
        exc[out] = (s1 >> 15) as i16;
        exc[out + 1] = (s2 >> 15) as i16;
        out += 2;
    }
}

// ---------------------------------------------------------------- algebraic codebooks

#[inline]
fn pulse_val(sign: i32) -> i16 {
    ((sign & 1) * 16383 - 8192) as i16
}

/// C `dec_10i40_35bits` (MR122 codebook). `index` has 10 entries; `cod` 40 samples.
#[inline]
pub fn dec_10i40_35bits(index: &[i16], cod: &mut [i16]) {
    for c in cod[..L_CODE as usize].iter_mut() {
        *c = 0;
    }
    for j in 0..NB_TRACK as usize {
        let tmp = index[j] as i32;
        let i = DGRAY[(tmp & 7) as usize] as i32;
        let pos1 = (i * 5) as usize + j;
        let mut sign: i16 = if (tmp >> 3) & 1 == 0 { 4096 } else { -4096 };
        cod[pos1] = sign;
        let i = DGRAY[(index[j + 5] & 7) as usize] as i32;
        let pos2 = (i * 5) as usize + j;
        if pos2 < pos1 {
            sign = negate(sign);
        }
        cod[pos2] = cod[pos2].wrapping_add(sign);
    }
}

/// C `decode_2i40_9bits` (MR515-ish 2 pulses, 9 bits). Uses table STARTPOS.
#[inline]
pub fn decode_2i40_9bits(sub_nr: i16, sign: i16, index: i16, cod: &mut [i16]) {
    let mut index = index as i32;
    let mut j = index & 64;
    j >>= 3;
    let mut i = index & 7;
    let mut k = (shl(sub_nr, 1) as i32 + j) as usize;
    let p0 = i * 5 + STARTPOS[k] as i32;
    k += 1;
    index >>= 3;
    i = index & 7;
    let p1 = i * 5 + STARTPOS[k] as i32;
    for c in cod[..L_SUBFR_].iter_mut() {
        *c = 0;
    }
    let mut sign = sign as i32;
    for p in [p0, p1] {
        cod[p as usize] = pulse_val(sign);
        sign >>= 1;
    }
}

/// C `decode_2i40_11bits`.
#[inline]
pub fn decode_2i40_11bits(sign: i16, index: i16, cod: &mut [i16]) {
    let mut index = index as i32;
    let mut j = index & 1;
    index >>= 1;
    let mut i = index & 7;
    let p0 = i * 5 + j * 2 + 1;
    index >>= 3;
    j = index & 3;
    index >>= 2;
    i = index & 7;
    let p1 = if j == 3 { i * 5 + 4 } else { i * 5 + j };
    for c in cod[..L_SUBFR_].iter_mut() {
        *c = 0;
    }
    let mut sign = sign as i32;
    for p in [p0, p1] {
        cod[p as usize] = pulse_val(sign);
        sign >>= 1;
    }
}

/// C `decode_3i40_14bits`.
#[inline]
pub fn decode_3i40_14bits(sign: i16, index: i16, cod: &mut [i16]) {
    let mut index = index as i32;
    let mut i = index & 7;
    let p0 = i * 5;
    index >>= 3;
    let mut j = index & 1;
    index >>= 1;
    i = index & 7;
    let p1 = i * 5 + j * 2 + 1;
    index >>= 3;
    j = index & 1;
    index >>= 1;
    i = index & 7;
    let p2 = i * 5 + j * 2 + 2;
    for c in cod[..L_SUBFR_].iter_mut() {
        *c = 0;
    }
    let mut sign = sign as i32;
    for p in [p0, p1, p2] {
        cod[p as usize] = pulse_val(sign);
        sign >>= 1;
    }
}

/// C `decode_4i40_17bits`.
#[inline]
pub fn decode_4i40_17bits(sign: i16, index: i16, cod: &mut [i16]) {
    let mut index = index as i32;
    let g = |x: i32| DGRAY[(x & 7) as usize] as i32;
    let p0 = g(index) * 5;
    index >>= 3;
    let p1 = g(index) * 5 + 1;
    index >>= 3;
    let p2 = g(index) * 5 + 2;
    index >>= 3;
    let j = index & 1;
    index >>= 1;
    let p3 = g(index) * 5 + 3 + j;
    for c in cod[..L_SUBFR_].iter_mut() {
        *c = 0;
    }
    let mut sign = sign as i32;
    for p in [p0, p1, p2, p3] {
        cod[p as usize] = pulse_val(sign);
        sign >>= 1;
    }
}

fn decompress10(msbs: i32, lsbs: i32, index1: usize, index2: usize, index3: usize, pos: &mut [i32; 8]) {
    let msbs = if msbs > 124 { 124 } else { msbs };
    let mut ia = m15(msbs, 1311);
    ia = msbs - ia * 25;
    let mut ib = m15(ia, 6554);
    ib = ia - ib * 5;
    ib <<= 1;
    let ic = lsbs - ((lsbs >> 2) << 2);
    pos[index1] = ib + (ic & 1);
    ib = m15(ia, 6554);
    ib <<= 1;
    pos[index2] = ib + (ic >> 1);
    ib = lsbs >> 2;
    let ic = m15(msbs, 1311) << 1;
    pos[index3] = ib + ic;
}

/// C `dec_8i40_31bits` (MR102 codebook). `index` has 7 entries; `cod` 40 samples.
#[inline]
pub fn dec_8i40_31bits(index: &[i16], cod: &mut [i16]) {
    const NT: usize = 4;
    for c in cod[..L_CODE as usize].iter_mut() {
        *c = 0;
    }
    let mut signs = [0i32; NT];
    let mut pos = [0i32; 8];
    for i in 0..NT {
        signs[i] = index[i] as i32;
    }
    let v = index[NT] as i32;
    decompress10(v >> 3, v & 7, 0, 4, 1, &mut pos);
    let v = index[NT + 1] as i32;
    decompress10(v >> 3, v & 7, 2, 6, 5, &mut pos);
    let v = index[NT + 2] as i32;
    let msbs = v >> 2;
    let lsbs = v & 3;
    let mut ia = msbs * 25;
    ia += 12;
    let msbs0_24 = ia >> 5;
    ia = m15(msbs0_24, 6554) & 1;
    let mut ib = m15(msbs0_24, 6554);
    ib = msbs0_24 - ib * 5;
    if ia == 1 {
        ib = 4 - ib;
    }
    ib <<= 1;
    pos[3] = ib + (lsbs & 1);
    ia = m15(msbs0_24, 6554) << 1;
    pos[7] = ia + (lsbs >> 1);

    for j in 0..NT {
        let pos1 = (pos[j] << 2) + j as i32;
        let mut sign: i16 = if signs[j] == 0 { 8191 } else { -8191 };
        if pos1 < 40 {
            cod[pos1 as usize] = sign;
        }
        let pos2 = (pos[j + 4] << 2) + j as i32;
        if pos2 < pos1 {
            sign = negate(sign);
        }
        if pos2 < 40 {
            cod[pos2 as usize] = cod[pos2 as usize].wrapping_add(sign);
        }
    }
}

// ---------------------------------------------------------------- gain predictor

const NPRED: usize = 4;
const MEAN_ENER_MR122: i32 = 783741;
const MIN_ENERGY: i16 = -14336;
const MIN_ENERGY_MR122: i16 = -2381;
const PRED: [i32; NPRED] = [5571, 4751, 2785, 1556];
const PRED_MR122: [i32; NPRED] = [44, 37, 22, 12];

/// C `gc_predState`.
#[derive(Clone, Debug)]
pub struct GcPredState {
    pub past_qua_en: [i16; 4],
    pub past_qua_en_mr122: [i16; 4],
}

impl GcPredState {
    pub fn new() -> Self {
        GcPredState {
            past_qua_en: [MIN_ENERGY; 4],
            past_qua_en_mr122: [MIN_ENERGY_MR122; 4],
        }
    }
    /// C `gc_pred_reset`.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for GcPredState {
    fn default() -> Self {
        Self::new()
    }
}

/// C `gc_pred`: MA prediction of the innovation gain. `code` = 40-sample innovation.
/// Outputs exp/frac of gcode0. `en` = Some((&mut exp_en, &mut frac_en)) receives the
/// innovation energy (only written in MR795 mode; C passes NULL otherwise).
#[inline]
pub fn gc_pred(
    st: &GcPredState,
    mode: Mode,
    code: &[i16],
    exp_gcode0: &mut i16,
    frac_gcode0: &mut i16,
    en: Option<(&mut i16, &mut i16)>,
) {
    let mut ener_code: i32 = 0;
    for &c in &code[..L_SUBFR_] {
        let t = c as i32;
        ener_code = ener_code.wrapping_add((t * t) >> 3);
    }
    ener_code = ener_code.wrapping_shl(4);
    if ener_code < 0 {
        ener_code = i32::MAX;
    }
    if mode == Mode::Mr122 {
        ener_code = (round_(ener_code) as i32 * 26214) << 1;
        let mut exp = 0i16;
        let mut frac = 0i16;
        log2(ener_code, &mut exp, &mut frac);
        let l1 = ((exp as i32 - 30) as i32).wrapping_shl(16);
        ener_code = l1.wrapping_add((frac as i32) << 1);
        let mut ener = MEAN_ENER_MR122;
        for i in 0..NPRED {
            let l = ((st.past_qua_en_mr122[i] as i32) * PRED_MR122[i]) << 1;
            ener = l_add(ener, l);
        }
        let mut l1 = l_sub(ener, ener_code);
        *exp_gcode0 = (l1 >> 17) as i16;
        let l2 = (*exp_gcode0 as i32).wrapping_shl(15);
        l1 >>= 2;
        *frac_gcode0 = l1.wrapping_sub(l2) as i16;
    } else {
        let exp_code = norm_l(ener_code);
        ener_code = l_shl(ener_code, exp_code);
        let mut exp = 0i16;
        let mut frac = 0i16;
        log2_norm(ener_code, exp_code, &mut exp, &mut frac);
        let l2 = (exp as i32 * -24660) << 1;
        let mut l_tmp = (frac as i32 * -24660) >> 15;
        if l_tmp & 0x0001_0000 != 0 {
            l_tmp |= 0xffff_0000u32 as i32;
        }
        l_tmp <<= 1;
        l_tmp = l_add(l_tmp, l2);
        match mode {
            Mode::Mr102 => {
                l_tmp = l_add(l_tmp, 16678 << 7);
            }
            Mode::Mr795 => {
                if let Some((e, f)) = en {
                    *f = (ener_code >> 16) as i16;
                    *e = (-11i32 - exp_code as i32) as i16;
                }
                l_tmp = l_add(l_tmp, 17062 << 7);
            }
            Mode::Mr74 => {
                l_tmp = l_add(l_tmp, 32588 << 6);
            }
            Mode::Mr67 => {
                l_tmp = l_add(l_tmp, 32268 << 6);
            }
            _ => {
                l_tmp = l_add(l_tmp, 16678 << 7);
            }
        }
        if l_tmp > 0x001f_ffff {
            l_tmp = i32::MAX;
        } else if l_tmp < -2097152 {
            l_tmp = i32::MIN;
        } else {
            l_tmp <<= 10;
        }
        for i in 0..4 {
            let l2 = ((PRED[i] * st.past_qua_en[i] as i32)) << 1;
            l_tmp = l_add(l_tmp, l2);
        }
        let gcode0 = (l_tmp >> 16) as i16;
        let mut l_tmp: i32 = if mode == Mode::Mr74 {
            (gcode0 as i32 * 5439) << 1
        } else {
            (gcode0 as i32 * 5443) << 1
        };
        l_tmp >>= 8;
        // (arithmetic shift of negatives equals the C ~((~x)>>8) form)
        *exp_gcode0 = (l_tmp >> 16) as i16;
        let l1 = l_tmp >> 1;
        let l2 = (*exp_gcode0 as i32).wrapping_shl(15);
        *frac_gcode0 = l_sub(l1, l2) as i16;
    }
}

/// C `gc_pred_update`: shift in the new quantised energies.
#[inline]
pub fn gc_pred_update(st: &mut GcPredState, qua_ener_mr122: i16, qua_ener: i16) {
    st.past_qua_en[3] = st.past_qua_en[2];
    st.past_qua_en_mr122[3] = st.past_qua_en_mr122[2];
    st.past_qua_en[2] = st.past_qua_en[1];
    st.past_qua_en_mr122[2] = st.past_qua_en_mr122[1];
    st.past_qua_en[1] = st.past_qua_en[0];
    st.past_qua_en_mr122[1] = st.past_qua_en_mr122[0];
    st.past_qua_en_mr122[0] = qua_ener_mr122;
    st.past_qua_en[0] = qua_ener;
}

/// C `gc_pred_average_limited`: returns `(ener_avg_MR122, ener_avg)`.
#[inline]
pub fn gc_pred_average_limited(st: &GcPredState) -> (i16, i16) {
    let avg = |v: &[i16; 4], min: i16| -> i16 {
        let mut a: i16 = 0;
        for &x in v {
            a = add(a, x);
        }
        a = if a < 0 { (a >> 2) | (0xc000u16 as i16) } else { a >> 2 };
        if a < min {
            min
        } else {
            a
        }
    };
    (
        avg(&st.past_qua_en_mr122, MIN_ENERGY_MR122),
        avg(&st.past_qua_en, MIN_ENERGY),
    )
}

// ---------------------------------------------------------------- gain decoding

/// C `Dec_gain`: joint gain decoding for MR475/515/59/67/74/102 (and 795 callers use d_gain_*).
/// Outputs `(gain_pit, gain_cod)` through the references.
#[inline]
pub fn dec_gain(
    pred_state: &mut GcPredState,
    mode: Mode,
    index: i16,
    code: &[i16],
    even_subfr: i16,
    gain_pit: &mut i16,
    gain_cod: &mut i16,
) {
    let mut index = shl(index, 2) as i32;
    let g_code: i16;
    let qua_ener_mr122: i16;
    let qua_ener: i16;
    if mode == Mode::Mr102 || mode == Mode::Mr74 || mode == Mode::Mr67 {
        let p = &TABLE_GAIN_HIGHRATES[index as usize..];
        *gain_pit = p[0];
        g_code = p[1];
        qua_ener_mr122 = p[2];
        qua_ener = p[3];
    } else if mode == Mode::Mr475 {
        index += (1 ^ even_subfr as i32) << 1;
        if index > (MR475_VQ_SIZE as i32 * 4 - 2) {
            index = MR475_VQ_SIZE as i32 * 4 - 2;
        }
        let p = &TABLE_GAIN_MR475[index as usize..];
        *gain_pit = p[0];
        g_code = p[1];
        let mut exp = 0i16;
        let mut frac = 0i16;
        log2(g_code as i32, &mut exp, &mut frac);
        exp = exp.wrapping_sub(12);
        let temp1 = shr_r(frac, 5);
        let temp2 = shl(exp, 10);
        qua_ener_mr122 = add(temp1, temp2);
        let l = mpy_32_16(exp, frac, 24660);
        let l = l_shl(l, 13);
        qua_ener = round_(l);
    } else {
        let p = &TABLE_GAIN_LOWRATES[index as usize..];
        *gain_pit = p[0];
        g_code = p[1];
        qua_ener_mr122 = p[2];
        qua_ener = p[3];
    }
    let mut exp = 0i16;
    let mut frac = 0i16;
    gc_pred(pred_state, mode, code, &mut exp, &mut frac, None);
    let gcode0 = pow2(14, frac) as i16;
    let l = l_mult(g_code, gcode0);
    let temp1 = 10i16.wrapping_sub(exp);
    let l = l_shr(l, temp1);
    *gain_cod = (l >> 16) as i16;
    gc_pred_update(pred_state, qua_ener_mr122, qua_ener);
}

/// C `d_gain_code`: decode innovation (fixed codebook) gain; returns gain_code.
#[inline]
pub fn d_gain_code(pred_state: &mut GcPredState, mode: Mode, index: i16, code: &[i16]) -> i16 {
    let mut exp = 0i16;
    let mut frac = 0i16;
    let mut exp_inn_en = 0i16;
    let mut frac_inn_en = 0i16;
    gc_pred(
        pred_state,
        mode,
        code,
        &mut exp,
        &mut frac,
        Some((&mut exp_inn_en, &mut frac_inn_en)),
    );
    let index = (index & 31) as usize;
    let p = &QUA_GAIN_CODE[index + (index << 1)..];
    let gain_code: i16;
    if mode == Mode::Mr122 {
        let mut gcode0 = pow2(exp, frac) as i16;
        gcode0 = shl(gcode0, 4);
        gain_code = shl(mult(gcode0, p[0]), 1);
    } else {
        let gcode0 = pow2(14, frac) as i16;
        let l = l_mult(p[0], gcode0);
        let l = l_shr(l, sub(9, exp));
        gain_code = (l >> 16) as i16;
    }
    let qua_ener_mr122 = p[1];
    let qua_ener = p[2];
    gc_pred_update(pred_state, qua_ener_mr122, qua_ener);
    gain_code
}

/// C `d_gain_pitch`: decode pitch gain.
#[inline]
pub fn d_gain_pitch(mode: Mode, index: i16) -> i16 {
    let mut gain = QUA_GAIN_PITCH[index as usize];
    if mode == Mode::Mr122 {
        gain &= 0xFFFCu16 as i16;
    }
    gain
}

// ---------------------------------------------------------------- phase dispersion

pub const PHDGAINMEMSIZE: usize = 5;
const PHDTHR1LTP: i16 = 9830;
const PHDTHR2LTP: i16 = 14746;
const ONFACTPLUS1: i32 = 16384;
const ONLENGTH: i16 = 2;

/// C `ph_dispState`.
#[derive(Clone, Debug)]
pub struct PhDispState {
    pub gain_mem: [i16; PHDGAINMEMSIZE],
    pub prev_state: i16,
    pub prev_cb_gain: i16,
    pub lock_full: i16,
    pub onset: i16,
}

impl PhDispState {
    pub fn new() -> Self {
        PhDispState {
            gain_mem: [0; PHDGAINMEMSIZE],
            prev_state: 0,
            prev_cb_gain: 0,
            lock_full: 0,
            onset: 0,
        }
    }
    /// C `ph_disp_reset`.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
    /// C `ph_disp_lock`.
    pub fn lock(&mut self) {
        self.lock_full = 1;
    }
    /// C `ph_disp_release`.
    pub fn release(&mut self) {
        self.lock_full = 0;
    }
}

impl Default for PhDispState {
    fn default() -> Self {
        Self::new()
    }
}

/// C `ph_disp_lock`.
#[inline]
pub fn ph_disp_lock(st: &mut PhDispState) {
    st.lock();
}

/// C `ph_disp_release`.
#[inline]
pub fn ph_disp_release(st: &mut PhDispState) {
    st.release();
}

/// C `ph_disp`: phase dispersion of the innovation and excitation construction.
/// `x` (40 samples, in/out): on entry the adapted LTP excitation, on exit the
/// total excitation. `inno` (40 samples, in/out): innovation (modified in place).
#[inline]
pub fn ph_disp(
    st: &mut PhDispState,
    mode: Mode,
    x: &mut [i16],
    cb_gain: i16,
    ltp_gain: i16,
    inno: &mut [i16],
    pitch_fac: i16,
    tmp_shift: i16,
) {
    st.gain_mem[4] = st.gain_mem[3];
    st.gain_mem[3] = st.gain_mem[2];
    st.gain_mem[2] = st.gain_mem[1];
    st.gain_mem[1] = st.gain_mem[0];
    st.gain_mem[0] = ltp_gain;

    let mut imp_nr: i16 = if ltp_gain < PHDTHR2LTP {
        if ltp_gain > PHDTHR1LTP {
            1
        } else {
            0
        }
    } else {
        2
    };

    let mut l_temp = (st.prev_cb_gain as i32 * ONFACTPLUS1) << 1;
    if l_temp > 0x1fff_ffff {
        l_temp = i32::MAX;
    } else if l_temp < -536870912 {
        l_temp = i32::MIN;
    } else {
        l_temp <<= 2;
    }
    let tmp1 = round_(l_temp);
    if cb_gain > tmp1 {
        st.onset = ONLENGTH;
    } else if st.onset > 0 {
        st.onset -= 1;
    }

    if st.onset == 0 {
        let mut i1 = 0;
        for i in 0..PHDGAINMEMSIZE {
            if st.gain_mem[i] < PHDTHR1LTP {
                i1 += 1;
            }
        }
        if i1 > 2 {
            imp_nr = 0;
        }
    }
    if imp_nr > st.prev_state + 1 && st.onset == 0 {
        imp_nr -= 1;
    }
    if imp_nr < 2 && st.onset > 0 {
        imp_nr += 1;
    }
    if cb_gain < 10 {
        imp_nr = 2;
    }
    if st.lock_full == 1 {
        imp_nr = 0;
    }
    st.prev_state = imp_nr;
    st.prev_cb_gain = cb_gain;

    if mode != Mode::Mr122 && mode != Mode::Mr102 && mode != Mode::Mr74 && imp_nr < 2 {
        let mut nze = 0usize;
        let mut ps_poss = [0usize; L_SUBFR_];
        let mut inno_sav = [0i16; L_SUBFR_];
        for i in 0..L_SUBFR_ {
            if inno[i] != 0 {
                ps_poss[nze] = i;
                nze += 1;
            }
            inno_sav[i] = inno[i];
            inno[i] = 0;
        }
        let ph_imp: &[i16] = if mode == Mode::Mr795 {
            if imp_nr == 0 {
                &PH_IMP_LOW_MR795
            } else {
                &PH_IMP_MID_MR795
            }
        } else if imp_nr == 0 {
            &PH_IMP_LOW
        } else {
            &PH_IMP_MID
        };
        for n in 0..nze {
            let ppos = ps_poss[n];
            let c = inno_sav[ppos] as i32;
            let mut pi = 0usize;
            for i in ppos..L_SUBFR_ {
                let t = ((c * ph_imp[pi] as i32) >> 15) as i16;
                pi += 1;
                inno[i] = add(inno[i], t);
            }
            for i in 0..ppos {
                let t = ((c * ph_imp[pi] as i32) >> 15) as i16;
                pi += 1;
                inno[i] = add(inno[i], t);
            }
        }
    }

    for i in 0..L_SUBFR_ {
        let mut l = l_mult(x[i], pitch_fac);
        let l2 = (inno[i] as i32).wrapping_mul(cb_gain as i32).wrapping_shl(1);
        l = l_add(l, l2);
        l = l_shl(l, tmp_shift);
        x[i] = round_(l);
    }
}

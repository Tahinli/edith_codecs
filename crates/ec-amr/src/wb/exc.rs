//! AMR-WB excitation / gain / concealment helpers (ports of the opencore-amr decoder files).
use super::exc_tab::*;
use super::ops::*;

const NB_POS4: i32 = 16;

/// Arithmetic right shift of an i16 by a C-style count (counts >= 16 give the sign fill).
#[inline]
fn sra16(v: i16, s: i32) -> i16 {
    // C promotes to int; the x86 shifter masks the count to 5 bits.
    v >> ((s & 31).min(15))
}

/// Ports `dec_acelp_2p_in_64`. `code` has 64 entries.
pub fn dec_acelp_2p_in_64(index: i16, code: &mut [i16]) {
    let index = index as i32;
    for c in code[..64].iter_mut() {
        *c = 0;
    }
    let i = ((index >> 5) & 0x003E) as usize;
    code[i] = if ((index >> 6) & 32) == 0 { 512 } else { -512 };
    let i = (((index & 0x001F) << 1) + 1) as usize;
    code[i] = if (index & 32) == 0 { 512 } else { -512 };
}

/// Ports `add_pulses` (dec_acelp_4p_in_64.cpp).
pub fn add_pulses(pos: &[i16], nb_pulse: i16, track: i16, code: &mut [i16]) {
    for k in 0..nb_pulse as usize {
        let p = pos[k] as i32;
        let i = (((p & (NB_POS4 - 1)) << 2) + track as i32) as usize;
        if (p & NB_POS4) == 0 {
            code[i] = code[i].wrapping_add(512);
        } else {
            code[i] = code[i].wrapping_sub(512);
        }
    }
}

/// Ports `dec_acelp_4p_in_64`. `index` has 8 entries, `code` 64.
pub fn dec_acelp_4p_in_64(index: &[i16], nbbits: i16, code: &mut [i16]) {
    let mut pos = [0i16; 6];
    for c in code[..64].iter_mut() {
        *c = 0;
    }
    match nbbits {
        20 => {
            for k in 0..4usize {
                dec_1p_N1(index[k] as i32, 4, 0, &mut pos);
                add_pulses(&pos, 1, k as i16, code);
            }
        }
        36 => {
            for k in 0..4usize {
                dec_2p_2N1(index[k] as i32, 4, 0, &mut pos);
                add_pulses(&pos, 2, k as i16, code);
            }
        }
        44 => {
            for k in 0..2usize {
                dec_3p_3N1(index[k] as i32, 4, 0, &mut pos);
                add_pulses(&pos, 3, k as i16, code);
            }
            for k in 2..4usize {
                dec_2p_2N1(index[k] as i32, 4, 0, &mut pos);
                add_pulses(&pos, 2, k as i16, code);
            }
        }
        52 => {
            for k in 0..4usize {
                dec_3p_3N1(index[k] as i32, 4, 0, &mut pos);
                add_pulses(&pos, 3, k as i16, code);
            }
        }
        64 => {
            for k in 0..4usize {
                let l = ((index[k] as i32) << 14).wrapping_add(index[k + 4] as i32);
                dec_4p_4N(l, 4, 0, &mut pos);
                add_pulses(&pos, 4, k as i16, code);
            }
        }
        72 => {
            for k in 0..2usize {
                let l = ((index[k] as i32) << 10).wrapping_add(index[k + 4] as i32);
                dec_5p_5N(l, 4, 0, &mut pos);
                add_pulses(&pos, 5, k as i16, code);
            }
            for k in 2..4usize {
                let l = ((index[k] as i32) << 14).wrapping_add(index[k + 4] as i32);
                dec_4p_4N(l, 4, 0, &mut pos);
                add_pulses(&pos, 4, k as i16, code);
            }
        }
        88 => {
            for k in 0..4usize {
                let l = ((index[k] as i32) << 11).wrapping_add(index[k + 4] as i32);
                dec_6p_6N_2(l, 4, 0, &mut pos);
                add_pulses(&pos, 6, k as i16, code);
            }
        }
        _ => {}
    }
}

#[inline]
fn shr(i: i32, n: i32) -> i32 {
    i.wrapping_shr(n as u32)
}

/// Ports `dec_1p_N1`.
pub fn dec_1p_N1(index: i32, n: i16, offset: i16, pos: &mut [i16]) {
    let n = n as i32;
    let mask = (1i32 << n) - 1;
    let mut pos1 = ((index & mask) + offset as i32) as i16;
    let i = shr(index, n) & 1;
    if i == 1 {
        pos1 = pos1.wrapping_add(NB_POS4 as i16);
    }
    pos[0] = pos1;
}

/// Ports `dec_2p_2N1`.
pub fn dec_2p_2N1(index: i32, n: i16, offset: i16, pos: &mut [i16]) {
    let mask = sub_int16(shl_int16(1, n), 1) as i32;
    let mut pos1 = add_int32(shr_int32(index, n) & mask, offset as i32) as i16;
    let tmp = shl_int16(n, 1);
    let i = shr(index, tmp as i32) & 1;
    let mut pos2 = add_int16((index & mask) as i16, offset);
    if pos2 < pos1 {
        if i == 1 {
            pos1 = pos1.wrapping_add(NB_POS4 as i16);
        } else {
            pos2 = pos2.wrapping_add(NB_POS4 as i16);
        }
    } else if i == 1 {
        pos1 = pos1.wrapping_add(NB_POS4 as i16);
        pos2 = pos2.wrapping_add(NB_POS4 as i16);
    }
    pos[0] = pos1;
    pos[1] = pos2;
}

/// Ports `dec_3p_3N1`.
pub fn dec_3p_3N1(index: i32, n: i16, offset: i16, pos: &mut [i16]) {
    let n32 = n as i32;
    let mask = (1i32 << ((2 * n32) - 1)) - 1;
    let mut idx = index & mask;
    let mut j = offset;
    let tmp = (n32 << 1) - 1;
    if (shr(index, tmp) & 1) != 0 {
        j = j.wrapping_add((1i32 << (n32 - 1)) as i16);
    }
    dec_2p_2N1(idx, n - 1, j, pos);
    let mask = (1i32 << (n32 + 1)) - 1;
    idx = shr(index, n32 << 1) & mask;
    dec_1p_N1(idx, n, offset, &mut pos[2..]);
}

/// Ports `dec_4p_4N1`.
pub fn dec_4p_4N1(index: i32, n: i16, offset: i16, pos: &mut [i16]) {
    let n32 = n as i32;
    let tmp = (n32 << 1) - 1;
    let mask = (1i32 << tmp) - 1;
    let mut idx = index & mask;
    let mut j = offset;
    if (shr(index, tmp) & 1) != 0 {
        j = j.wrapping_add((1i32 << (n32 - 1)) as i16);
    }
    dec_2p_2N1(idx, n - 1, j, pos);
    let tmp = (n32 << 1) + 1;
    let mask = (1i32 << tmp) - 1;
    idx = shr(index, n32 << 1) & mask;
    dec_2p_2N1(idx, n, offset, &mut pos[2..]);
}

/// Ports `dec_4p_4N`.
pub fn dec_4p_4N(index: i32, n: i16, offset: i16, pos: &mut [i16]) {
    let n_1 = n - 1;
    let n1 = n_1 as i32;
    let j = offset.wrapping_add((1i32 << n1) as i16);
    let tmp = ((n as i32) << 2) - 2;
    match shr(index, tmp) & 3 {
        0 => {
            let t = (n1 << 2) + 1;
            if (shr(index, t) & 1) != 0 {
                dec_4p_4N1(index, n_1, j, pos);
            } else {
                dec_4p_4N1(index, n_1, offset, pos);
            }
        }
        1 => {
            let t = 3 * n1 + 1;
            dec_1p_N1(shr(index, t), n_1, offset, pos);
            dec_3p_3N1(index, n_1, j, &mut pos[1..]);
        }
        2 => {
            let t = (n1 << 1) + 1;
            dec_2p_2N1(shr(index, t), n_1, offset, pos);
            dec_2p_2N1(index, n_1, j, &mut pos[2..]);
        }
        _ => {
            let t = n1 + 1;
            dec_3p_3N1(shr(index, t), n_1, offset, pos);
            dec_1p_N1(index, n_1, j, &mut pos[3..]);
        }
    }
}

/// Ports `dec_5p_5N`.
pub fn dec_5p_5N(index: i32, n: i16, offset: i16, pos: &mut [i16]) {
    let n32 = n as i32;
    let n_1 = n - 1;
    let j = add_int16(offset, shl_int16(1, n_1));
    let idx = shr(index, (n32 << 1) + 1);
    let tmp = 5 * n32 - 1;
    if (shr(index, tmp) & 1) != 0 {
        dec_3p_3N1(idx, n_1, j, pos);
        dec_2p_2N1(index, n, offset, &mut pos[3..]);
    } else {
        dec_3p_3N1(idx, n_1, offset, pos);
        dec_2p_2N1(index, n, offset, &mut pos[3..]);
    }
}

/// Ports `dec_6p_6N_2`.
pub fn dec_6p_6N_2(index: i32, n: i16, offset: i16, pos: &mut [i16]) {
    let n32 = n as i32;
    let n_1 = n - 1;
    let n1 = n_1 as i32;
    let j = offset.wrapping_add((1i32 << n1) as i16);
    let mut offset_a = j;
    let mut offset_b = j;
    if (shr(index, 6 * n32 - 5) & 1) == 0 {
        offset_a = offset;
    } else {
        offset_b = offset;
    }
    match shr(index, 6 * n32 - 4) & 3 {
        0 => {
            dec_5p_5N(shr(index, n32), n_1, offset_a, pos);
            dec_1p_N1(index, n_1, offset_a, &mut pos[5..]);
        }
        1 => {
            dec_5p_5N(shr(index, n32), n_1, offset_a, pos);
            dec_1p_N1(index, n_1, offset_b, &mut pos[5..]);
        }
        2 => {
            dec_4p_4N(shr(index, 2 * n1 + 1), n_1, offset_a, pos);
            dec_2p_2N1(index, n_1, offset_b, &mut pos[4..]);
        }
        _ => {
            dec_3p_3N1(shr(index, 3 * n1 + 1), n_1, offset, pos);
            dec_3p_3N1(index, n_1, j, &mut pos[3..]);
        }
    }
}

/// Ports `Pred_lt4`. `exc[base]` is the C `exc[0]`; history is read at `base - T0 ...`.
pub fn pred_lt4(exc: &mut [i16], base: usize, t0: i16, frac: i16, l_subfr: i16) {
    let mut frac = (-(frac as i32)) as i16;
    let mut pt: isize = base as isize - t0 as isize;
    if frac < 0 {
        frac += 4;
        pt -= 1;
    }
    pt -= 15;
    let h = &INTER4_2[(3 - frac) as usize];
    let mut j = 0usize;
    while j < (l_subfr >> 2) as usize {
        let p = pt as usize;
        let mut s1: i32 = 0x2000;
        let mut s2: i32 = 0x2000;
        let mut s3: i32 = 0x2000;
        let mut s4: i32 = 0x2000;
        let mut i = 0usize;
        while i < 32 {
            let mut t1 = exc[p + i];
            let mut t2 = exc[p + i + 1];
            let t3 = exc[p + i + 2];
            s1 = fxp_mac_16by16(t1, h[i], s1);
            s2 = fxp_mac_16by16(t2, h[i], s2);
            s1 = fxp_mac_16by16(t2, h[i + 1], s1);
            s2 = fxp_mac_16by16(t3, h[i + 1], s2);
            s3 = fxp_mac_16by16(t3, h[i], s3);
            s1 = fxp_mac_16by16(t3, h[i + 2], s1);
            t1 = exc[p + i + 3];
            t2 = exc[p + i + 4];
            s4 = fxp_mac_16by16(t1, h[i], s4);
            s3 = fxp_mac_16by16(t1, h[i + 1], s3);
            s2 = fxp_mac_16by16(t1, h[i + 2], s2);
            s1 = fxp_mac_16by16(t1, h[i + 3], s1);
            s4 = fxp_mac_16by16(t2, h[i + 1], s4);
            s2 = fxp_mac_16by16(t2, h[i + 3], s2);
            s3 = fxp_mac_16by16(t2, h[i + 2], s3);
            t1 = exc[p + i + 5];
            t2 = exc[p + i + 6];
            s4 = fxp_mac_16by16(t1, h[i + 2], s4);
            s3 = fxp_mac_16by16(t1, h[i + 3], s3);
            s4 = fxp_mac_16by16(t2, h[i + 3], s4);
            i += 4;
        }
        exc[base + (j << 2)] = (s1 >> 14) as i16;
        exc[base + (j << 2) + 1] = (s2 >> 14) as i16;
        exc[base + (j << 2) + 2] = (s3 >> 14) as i16;
        exc[base + (j << 2) + 3] = (s4 >> 14) as i16;
        pt += 4;
        j += 1;
    }
    if (l_subfr & 1) != 0 {
        let p = pt as usize;
        let mut s1: i32 = 0x2000;
        let mut i = 0usize;
        while i < 32 {
            s1 = fxp_mac_16by16(exc[p + i], h[i], s1);
            s1 = fxp_mac_16by16(exc[p + i + 1], h[i + 1], s1);
            s1 = fxp_mac_16by16(exc[p + i + 2], h[i + 2], s1);
            s1 = fxp_mac_16by16(exc[p + i + 3], h[i + 3], s1);
            i += 4;
        }
        exc[base + (j << 2)] = (s1 >> 14) as i16;
    }
}

/// Ports `Pit_shrp`.
pub fn pit_shrp(x: &mut [i16], pit_lag: i16, sharp: i16, l_subfr: i16) {
    let mut i = pit_lag as usize;
    while (i as i32) < l_subfr as i32 {
        let l = mac_16by16_to_int32((x[i] as i32) << 16, x[i - pit_lag as usize], sharp);
        x[i] = amr_wb_round(l);
        i += 1;
    }
}

/// Ports `preemph_amrwb_dec`.
pub fn preemph_amrwb_dec(x: &mut [i16], mu: i16, lg: i16) {
    let mut i = lg as i32 - 1;
    while i != 0 {
        let iu = i as usize;
        let l = msu_16by16_from_int32((x[iu] as i32) << 16, x[iu - 1], mu);
        x[iu] = amr_wb_round(l);
        i -= 1;
    }
}

const PITCH_0_9: i16 = 14746;
const PITCH_0_6: i16 = 9830;

/// Ports `phase_dispersion`. `disp_mem` has 8 words, `scratch` at least 128.
pub fn phase_dispersion(
    gain_code: i16,
    gain_pit: i16,
    code: &mut [i16],
    mode: i16,
    disp_mem: &mut [i16],
    scratch: &mut [i16],
) {
    let code2 = &mut scratch[..128];
    for c in code2.iter_mut() {
        *c = 0;
    }
    let mut state: i16 = if gain_pit < PITCH_0_6 {
        0
    } else if gain_pit < PITCH_0_9 {
        1
    } else {
        2
    };
    // prev_state = disp_mem[0], prev_gain_code = disp_mem[1], prev_gain_pit = disp_mem[2..8]
    for i in (1..=5).rev() {
        disp_mem[2 + i] = disp_mem[2 + i - 1];
    }
    disp_mem[2] = gain_pit;
    if sub_int16(gain_code, disp_mem[1]) > shl_int16(disp_mem[1], 1) {
        if state < 2 {
            state += 1;
        }
    } else {
        let mut j = 0;
        for i in 0..6 {
            if disp_mem[2 + i] < PITCH_0_6 {
                j += 1;
            }
        }
        if j > 2 {
            state = 0;
        }
        if state > disp_mem[0] + 1 {
            state -= 1;
        }
    }
    disp_mem[1] = gain_code;
    disp_mem[0] = state;

    state = state.wrapping_add(mode);
    if state == 0 || state == 1 {
        let tab: &[i16; 64] = if state == 0 { &PH_IMP_LOW } else { &PH_IMP_MID };
        for i in 0..64usize {
            if code[i] != 0 {
                for j in 0..64usize {
                    code2[i + j] =
                        add_int16(code2[i + j], super::math::mult_int16_r(code[i], tab[j]));
                }
            }
        }
    }
    if state < 2 {
        for i in 0..64usize {
            code[i] = add_int16(code2[i], code2[i + 64]);
        }
    }
}

/// Ports `noise_gen_amrwb`.
pub fn noise_gen_amrwb(seed: &mut i16) -> i16 {
    *seed = (*seed as i32 * 31821).wrapping_add(13849) as i16;
    *seed
}

/// Ports `voice_factor`.
pub fn voice_factor(
    exc: &[i16],
    q_exc: i16,
    gain_pit: i16,
    code: &[i16],
    gain_code: i16,
    l_subfr: i16,
) -> i16 {
    let mut exp1: i16 = 0;
    let mut exp2: i16 = 0;
    let mut ener1 =
        super::math::extract_h(super::math::dot_product12(exc, exc, l_subfr, &mut exp1));
    exp1 = sub_int16(exp1, ((q_exc as i32) << 1) as i16);
    let l_tmp = mul_16by16_to_int32(gain_pit, gain_pit);
    let mut exp = normalize_amr_wb(l_tmp);
    let mut tmp = (l_tmp.wrapping_shl(exp as u32) >> 16) as i16;
    ener1 = mult_int16(ener1, tmp);
    exp1 = exp1.wrapping_sub(exp.wrapping_add(10));
    let mut ener2 =
        super::math::extract_h(super::math::dot_product12(code, code, l_subfr, &mut exp2));
    exp = norm_s(gain_code);
    tmp = shl_int16(gain_code, exp);
    tmp = mult_int16(tmp, tmp);
    ener2 = mult_int16(ener2, tmp);
    exp2 = exp2.wrapping_sub(exp.wrapping_shl(1));
    let i = exp1.wrapping_sub(exp2) as i32;
    if i >= 0 {
        ener1 = sra16(ener1, 1);
        ener2 = sra16(ener2, i + 1);
    } else {
        ener1 = sra16(ener1, 1 - i);
        ener2 = sra16(ener2, 1);
    }
    tmp = ener1.wrapping_sub(ener2);
    ener1 = ener1.wrapping_add(ener2).wrapping_add(1);
    if tmp >= 0 {
        super::math::div_16by16(tmp, ener1)
    } else {
        negate_int16(super::math::div_16by16(negate_int16(tmp), ener1))
    }
}

/// Ports `agc2_amr_wb`.
pub fn agc2_amr_wb(sig_in: &[i16], sig_out: &mut [i16], l_trm: i16) {
    let l = l_trm as usize;
    let mut temp = sig_out[0] >> 2;
    let mut s: i32 = fxp_mul_16by16(temp, temp).wrapping_shl(1);
    for i in 1..l {
        temp = sig_out[i] >> 2;
        s = mac_16by16_to_int32(s, temp, temp);
    }
    if s == 0 {
        return;
    }
    let mut exp = normalize_amr_wb(s) - 1;
    let gain_out = amr_wb_round(s.wrapping_shl(exp as u32));
    temp = sig_in[0] >> 2;
    s = mul_16by16_to_int32(temp, temp);
    for i in 1..l {
        temp = sig_in[i] >> 2;
        s = mac_16by16_to_int32(s, temp, temp);
    }
    let g0: i16;
    if s == 0 {
        g0 = 0;
    } else {
        let i = normalize_amr_wb(s);
        let gain_in = amr_wb_round(s.wrapping_shl(i as u32));
        exp = exp.wrapping_sub(i);
        let mut s2 = super::math::div_16by16(gain_out, gain_in) as i32;
        s2 = shl_int32(s2, 7);
        s2 = shr_int32(s2, exp);
        s2 = super::math::one_ov_sqrt(s2);
        g0 = amr_wb_round(shl_int32(s2, 9));
    }
    for i in 0..l {
        sig_out[i] = super::math::extract_h(shl_int32(fxp_mul_16by16(sig_out[i], g0), 3));
    }
}

/// Ports `scale_signal`.
pub fn scale_signal(x: &mut [i16], lg: i16, exp: i16) {
    if exp > 0 {
        for i in 0..lg as usize {
            let l = shl_int32((x[i] as i32) << 16, exp);
            x[i] = amr_wb_round(l);
        }
    } else if exp < 0 {
        let e = ((-(exp as i32)) & 0xf) as u32;
        let tmp = (0x8000i32 >> (16 - e)) as i16;
        for i in 0..(lg >> 1) as usize * 2 {
            x[i] = add_int16(x[i], tmp) >> e;
        }
    }
}

/// Ports `median5`. `x[c]` is the C `x[0]` (reads `x[c-2..=c+2]`).
#[allow(unused_assignments)]
pub fn median5(x: &[i16], c: usize) -> i16 {
    let mut x1 = x[c - 2];
    let mut x2 = x[c - 1];
    let mut x3 = x[c];
    let mut x4 = x[c + 1];
    let mut x5 = x[c + 2];
    let mut tmp: i16;
    if x2 < x1 {
        tmp = x1;
        x1 = x2;
        x2 = tmp;
    }
    if x3 < x1 {
        tmp = x1;
        x1 = x3;
        x3 = tmp;
    }
    if x4 < x1 {
        tmp = x1;
        x1 = x4;
        x4 = tmp;
    }
    if x5 < x1 {
        x5 = x1;
    }
    if x3 < x2 {
        tmp = x2;
        x2 = x3;
        x3 = tmp;
    }
    if x4 < x2 {
        tmp = x2;
        x2 = x4;
        x4 = tmp;
    }
    if x5 < x2 {
        x5 = x2;
    }
    if x4 < x3 {
        x3 = x4;
    }
    if x5 < x3 {
        x3 = x5;
    }
    x3
}

const L_LTPHIST: usize = 5;
const ONE_PER_3: i16 = 10923;
const ONE_PER_LTPHIST: i16 = 6554;

/// Ports `Init_Lagconc`.
pub fn init_lagconc(lag_hist: &mut [i16]) {
    for i in 0..L_LTPHIST {
        lag_hist[i] = 64;
    }
}

fn insert(array: &mut [i16], n: i16, x: i16) {
    let mut i = n as i32 - 1;
    while i >= 0 {
        if x < array[i as usize] {
            array[i as usize + 1] = array[i as usize];
        } else {
            break;
        }
        i -= 1;
    }
    array[(i + 1) as usize] = x;
}

fn insertion_sort(array: &mut [i16], n: i16) {
    for i in 0..n {
        let x = array[i as usize];
        insert(array, i, x);
    }
}

fn lag_random(lag_hist: &[i16], seed: &mut i16) -> i16 {
    let mut lag_hist2 = [0i16; L_LTPHIST];
    lag_hist2.copy_from_slice(&lag_hist[..L_LTPHIST]);
    insertion_sort(&mut lag_hist2, 5);
    let mut lag_dif = sub_int16(lag_hist2[4], lag_hist2[2]);
    if lag_dif > 40 {
        lag_dif = 40;
    }
    let d = noise_gen_amrwb(seed);
    let tmp = lag_dif >> 1;
    let d2 = mult_int16(tmp, d);
    let tmp = add_int16(add_int16(lag_hist2[2], lag_hist2[3]), lag_hist2[4]);
    add_int16(mult_int16(tmp, ONE_PER_3), d2)
}

/// Ports `lagconceal`.
pub fn lagconceal(
    gain_hist: &[i16],
    lag_hist: &[i16],
    t0: &mut i16,
    old_t0: &i16,
    seed: &mut i16,
    unusable_frame: i16,
) {
    let last_gain = gain_hist[4];
    let sec_last_gain = gain_hist[3];
    let last_lag = lag_hist[0];
    let mut min_lag = lag_hist[0];
    let mut max_lag = lag_hist[0];
    for i in 1..L_LTPHIST {
        if lag_hist[i] < min_lag {
            min_lag = lag_hist[i];
        }
        if lag_hist[i] > max_lag {
            max_lag = lag_hist[i];
        }
    }
    let mut min_gain = gain_hist[0];
    for i in 1..L_LTPHIST {
        if gain_hist[i] < min_gain {
            min_gain = gain_hist[i];
        }
    }
    let lag_dif = sub_int16(max_lag, min_lag);

    if unusable_frame != 0 {
        if min_gain > 8192 && lag_dif < 10 {
            *t0 = *old_t0;
        } else if last_gain > 8192 && sec_last_gain > 8192 {
            *t0 = lag_hist[0];
        } else {
            *t0 = lag_random(lag_hist, seed);
        }
        if *t0 > max_lag {
            *t0 = max_lag;
        }
        if *t0 < min_lag {
            *t0 = min_lag;
        }
    } else {
        let mut mean_lag: i16 = 0;
        for i in 0..L_LTPHIST {
            mean_lag = add_int16(mean_lag, lag_hist[i]);
        }
        mean_lag = mult_int16(mean_lag, ONE_PER_LTPHIST);
        let t = *t0 as i32;
        let tmp = (t - max_lag as i32) as i16 as i32;
        let tmp2 = (t - last_lag as i32) as i16 as i32;
        if lag_dif < 10 && t > (min_lag as i32 - 5) && tmp < 5 {
        } else if last_gain > 8192 && sec_last_gain > 8192 && (tmp2 + 10) > 0 && tmp2 < 10 {
        } else if min_gain < 6554
            && last_gain == min_gain
            && t > min_lag as i32
            && t < max_lag as i32
        {
        } else if lag_dif < 70 && t > min_lag as i32 && t < max_lag as i32 {
        } else if t > mean_lag as i32 && t < max_lag as i32 {
        } else {
            if min_gain > 8192 && lag_dif < 10 {
                *t0 = lag_hist[0];
            } else if last_gain > 8192 && sec_last_gain > 8192 {
                *t0 = lag_hist[0];
            } else {
                *t0 = lag_random(lag_hist, seed);
            }
            if *t0 > max_lag {
                *t0 = max_lag;
            }
            if *t0 < min_lag {
                *t0 = min_lag;
            }
        }
    }
}

const MEAN_ENER: i16 = 30;
static PDOWN_UNUSABLE: [i16; 7] = [32767, 31130, 29491, 24576, 7537, 1638, 328];
static CDOWN_UNUSABLE: [i16; 7] = [32767, 16384, 8192, 8192, 8192, 4915, 3277];
static PDOWN_USABLE: [i16; 7] = [32767, 32113, 31457, 24576, 7537, 1638, 328];
static CDOWN_USABLE: [i16; 7] = [32767, 32113, 32113, 32113, 32113, 32113, 22938];
static PRED: [i16; 4] = [4096, 3277, 2458, 1638];

/// Ports `dec_gain2_amr_wb_init`. `mem` has 23 words.
pub fn dec_gain2_amr_wb_init(mem: &mut [i16]) {
    for i in 0..4 {
        mem[i] = -14336;
    }
    for i in 4..22 {
        mem[i] = 0;
    }
    mem[22] = 21845;
}

/// Ports `dec_gain2_amr_wb`. `mem` has 23 words (see init).
pub fn dec_gain2_amr_wb(
    index: i16,
    nbits: i16,
    code: &[i16],
    l_subfr: i16,
    gain_pit: &mut i16,
    gain_cod: &mut i32,
    bfi: i16,
    prev_bfi: i16,
    state: i16,
    unusable_frame: i16,
    vad_hist: i16,
    mem: &mut [i16],
) {
    // layout: qua_en 0..4, past_gain_pit 4, past_gain_code 5, prev_gc 6, pbuf 7.., gbuf 12.., pbuf2 17..
    const PGP: usize = 4;
    const PGC: usize = 5;
    const PREV_GC: usize = 6;
    const PBUF: usize = 7;
    const GBUF: usize = 12;
    const PBUF2: usize = 17;

    let mut exp: i16 = 0;
    let mut frac: i16 = 0;
    let mut l_tmp = super::math::dot_product12(code, code, l_subfr, &mut exp);
    exp = exp.wrapping_sub(24);
    super::math::one_ov_sqrt_norm(&mut l_tmp, &mut exp);
    let gcode_inov = super::math::extract_h(shl_int32(l_tmp, exp.wrapping_sub(3)));

    if bfi != 0 {
        let mut tmp = median5(mem, PBUF + 2);
        mem[PGP] = tmp;
        if mem[PGP] > 15565 {
            mem[PGP] = 15565;
        }
        if unusable_frame != 0 {
            *gain_pit = mult_int16(PDOWN_UNUSABLE[state as usize], mem[PGP]);
        } else {
            *gain_pit = mult_int16(PDOWN_USABLE[state as usize], mem[PGP]);
        }
        tmp = median5(mem, GBUF + 2);
        if vad_hist > 2 {
            mem[PGC] = tmp;
        } else if unusable_frame != 0 {
            mem[PGC] = mult_int16(CDOWN_UNUSABLE[state as usize], tmp);
        } else {
            mem[PGC] = mult_int16(CDOWN_USABLE[state as usize], tmp);
        }
        tmp = mem[3];
        let mut tmp1 = mem[2];
        let mut lt: i32 = tmp as i32;
        lt += tmp1 as i32;
        mem[3] = tmp;
        tmp = mem[1];
        tmp1 = mem[0];
        lt += tmp as i32;
        lt += tmp1 as i32;
        mem[2] = tmp;
        let mut qua_ener = (lt >> 3) as i16;
        mem[1] = tmp1;
        qua_ener = qua_ener.wrapping_sub(3072);
        if qua_ener < -14336 {
            qua_ener = -14336;
        }
        mem[0] = qua_ener;
        for i in 1..5 {
            mem[GBUF + i - 1] = mem[GBUF + i];
            mem[PBUF + i - 1] = mem[PBUF + i];
        }
        mem[GBUF + 4] = mem[PGC];
        mem[PBUF + 4] = mem[PGP];
        *gain_cod = mul_16by16_to_int32(mem[PGC], gcode_inov);
        return;
    }

    let mut l_tmp = super::math::l_deposit_h(MEAN_ENER);
    l_tmp = shl_int32(l_tmp, 8);
    for k in 0..4 {
        l_tmp = mac_16by16_to_int32(l_tmp, PRED[k], mem[k]);
    }
    let mut gcode0 = super::math::extract_h(l_tmp);
    let l_tmp = ((gcode0 as i32) * 5443) >> 7;
    let mut exp_gcode0: i16 = 0;
    (exp_gcode0, frac) = super::math::int32_to_dpf(l_tmp);
    gcode0 = super::math::power_of_2(14, frac) as i16;
    exp_gcode0 = exp_gcode0.wrapping_sub(14);

    let tab: &[i16] = if nbits == 6 {
        &T_QUA_GAIN6B
    } else {
        &T_QUA_GAIN7B
    };
    let p = (index as i32 as usize) << 1;
    *gain_pit = tab[p];
    let g_code = tab[p + 1];
    let mut l_tmp = mul_16by16_to_int32(g_code, gcode0);
    l_tmp = shl_int32(l_tmp, exp_gcode0.wrapping_add(4));
    *gain_cod = l_tmp;

    if prev_bfi == 1 {
        let lt = mul_16by16_to_int32(mem[PREV_GC], 5120);
        if *gain_cod > lt && *gain_cod > 6553600 {
            *gain_cod = lt;
        }
    }

    mem[PGC] = amr_wb_round(shl_int32(*gain_cod, 3));
    mem[PGP] = *gain_pit;
    mem[PREV_GC] = mem[PGC];
    let mut tmp = mem[GBUF + 1];
    let mut tmp1 = mem[PBUF + 1];
    let mut tmp2 = mem[PBUF2 + 1];
    for i in 1..5 {
        mem[GBUF + i - 1] = tmp;
        mem[PBUF + i - 1] = tmp1;
        mem[PBUF2 + i - 1] = tmp2;
        tmp = mem[GBUF + i];
        tmp1 = mem[PBUF + i];
        tmp2 = mem[PBUF2 + i];
    }
    mem[GBUF + 4] = mem[PGC];
    mem[PBUF + 4] = mem[PGP];
    mem[PBUF2 + 4] = mem[PGP];

    (exp, frac) = super::math::int32_to_dpf(*gain_cod);
    let lt = mul_32by16(exp, frac, gcode_inov);
    *gain_cod = shl_int32(lt, 3);
    mem[3] = mem[2];
    mem[2] = mem[1];
    mem[1] = mem[0];

    super::math::amrwb_log_2(g_code as i32, &mut exp, &mut frac);
    exp = exp.wrapping_sub(11);
    let lt = mul_32by16(exp, frac, 24660);
    mem[0] = (lt >> 3) as i16;
}

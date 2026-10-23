//! AMR-WB synthesis stage: ports of wb_syn_filt.cpp, synthesis_amr_wb.cpp,
//! oversamp_12k8_to_16k.cpp, highpass_50hz_at_12k8.cpp, highpass_400hz_at_12k8.cpp,
//! band_pass_6k_7k.cpp, low_pass_filt_7k.cpp, deemphasis_32.cpp, isf_extrapolation.cpp.
#![allow(dead_code)]

use super::ops::*;

const L_FIR_BP: usize = 30;
const L_FIR_LP: usize = 30;
const L_FILT16K_: usize = 15;
const SPEECH: i16 = 0;

pub static HP_GAIN: [i16; 16] = [
    3624, 4673, 5597, 6479, 7425, 8378, 9324, 10264, 11210, 12206, 13391, 14844, 16770, 19655,
    24289, 32728,
];
pub static FIR_UP: [[i16; 24]; 4] = [
    [
        -1, 12, -33, 68, -119, 191, -291, 430, -634, 963, -1616, 3792, 15317, -2496, 1288, -809,
        542, -369, 247, -160, 96, -52, 23, -6,
    ],
    [
        -4, 24, -62, 124, -213, 338, -510, 752, -1111, 1708, -2974, 8219, 12368, -3432, 1881,
        -1204, 812, -552, 368, -235, 139, -73, 30, -7,
    ],
    [
        -7, 30, -73, 139, -235, 368, -552, 812, -1204, 1881, -3432, 12368, 8219, -2974, 1708,
        -1111, 752, -510, 338, -213, 124, -62, 24, -4,
    ],
    [
        -6, 23, -52, 96, -160, 247, -369, 542, -809, 1288, -2496, 15317, 3792, -1616, 963, -634,
        430, -291, 191, -119, 68, -33, 12, -1,
    ],
];
pub static FIR_6K_7K: [i16; 30] = [
    -32, 47, 32, -27, -369, 1122, -1421, 0, 3798, -8880, 12349, -10984, 3548, 7766, -18001, 22118,
    -18001, 7766, 3548, -10984, 12349, -8880, 3798, 0, -1421, 1122, -369, -27, 32, 47,
];
pub static FIR_7K: [i16; 31] = [
    -21, 47, -89, 146, -203, 229, -177, 0, 335, -839, 1485, -2211, 2931, -3542, 3953, 28682, 3953,
    -3542, 2931, -2211, 1485, -839, 335, 0, -177, 229, -203, 146, -89, 47, -21,
];

/// Decoder_State fields used by synthesis_amr_wb and callees.
#[derive(Clone, Debug)]
pub struct SynthState {
    pub mem_syn_hi: [i16; M],
    pub mem_syn_lo: [i16; M],
    pub mem_deemph: i16,
    pub mem_sig_out: [i16; 6],
    pub mem_oversamp: [i16; 2 * L_FILT],
    pub mem_syn_hf: [i16; M16K],
    pub mem_hf: [i16; 2 * L_FILT16K],
    pub mem_hf3: [i16; 2 * L_FILT16K],
    pub mem_hp400: [i16; 6],
    pub seed2: i16,
    pub vad_hist: i16,
}

impl SynthState {
    /// Zeroed state with seed2 = RANDOM_INITSEED (as after pvDecoder_AmrWb_Init).
    pub fn new() -> Self {
        SynthState {
            mem_syn_hi: [0; M],
            mem_syn_lo: [0; M],
            mem_deemph: 0,
            mem_sig_out: [0; 6],
            mem_oversamp: [0; 2 * L_FILT],
            mem_syn_hf: [0; M16K],
            mem_hf: [0; 2 * L_FILT16K],
            mem_hf3: [0; 2 * L_FILT16K],
            mem_hp400: [0; 6],
            seed2: 21845,
            vad_hist: 0,
        }
    }

    /// Relevant part of pvDecoder_AmrWb_Reset: only does anything when reset_all != 0.
    pub fn reset(&mut self, reset_all: bool) {
        if reset_all {
            self.mem_oversamp = [0; 2 * L_FILT];
            self.mem_hf = [0; 2 * L_FILT16K];
            self.mem_hf3 = [0; 2 * L_FILT16K];
            self.mem_sig_out = [0; 6];
            self.mem_hp400 = [0; 6];
            self.mem_deemph = 0;
            self.seed2 = 21845;
            self.mem_syn_hf = [0; M16K];
            self.mem_syn_hi = [0; M];
            self.mem_syn_lo = [0; M];
            self.vad_hist = 0;
        }
    }
}

impl Default for SynthState {
    fn default() -> Self {
        Self::new()
    }
}

/// Ports wb_syn_filt (HF synthesis filter). `x` and `y` are separate here (C calls it in place);
/// `mem` has m entries, `y_buf` must hold at least m+lg entries.
pub fn wb_syn_filt(
    a: &[i16],
    m: i16,
    x: &[i16],
    y: &mut [i16],
    lg: i16,
    mem: &mut [i16],
    update: i16,
    y_buf: &mut [i16],
) {
    let m = m as usize;
    let lg = lg as usize;
    y_buf[..m].copy_from_slice(&mem[..m]);
    for n in 0..(lg >> 2) << 2 {
        let mut l: i32 = -((x[n] as i32) << 11);
        for t in 1..=m {
            l = fxp_mac_16by16(y_buf[m + n - t], a[t], l);
        }
        let l = shl_int32(l, 4);
        let v = amr_wb_round(l.wrapping_neg());
        y[n] = v;
        y_buf[m + n] = v;
    }
    if update != 0 {
        mem[..m].copy_from_slice(&y[lg - m..lg]);
    }
}

/// Ports Syn_filt_32. `sig_hi`/`sig_lo` are the full buffers: `m` history samples first,
/// the output region starts at index `m` (C passes `buf + M`).
pub fn syn_filt_32(
    a: &[i16],
    m: i16,
    exc: &[i16],
    q_new: i16,
    sig_hi: &mut [i16],
    sig_lo: &mut [i16],
    lg: i16,
) {
    let m = m as usize;
    let a0 = (9i16.wrapping_sub(q_new)) as u32;
    for n in 0..lg as usize {
        let mut lo_s: i32 = 0;
        let mut hi_s: i32 = 0;
        for k in 1..=m {
            lo_s = fxp_mac_16by16(sig_lo[m + n - k], a[k], lo_s);
            hi_s = fxp_mac_16by16(sig_hi[m + n - k], a[k], hi_s);
        }
        let mut t: i32 = lo_s.wrapping_neg() >> 11;
        t = t.wrapping_add((exc[n] as i32).wrapping_shl(a0));
        t = t.wrapping_sub(hi_s.wrapping_shl(1));
        t = shl_int32(t, 3);
        sig_hi[m + n] = (t >> 16) as i16;
        sig_lo[m + n] = ((t >> 4).wrapping_sub((t >> 16).wrapping_shl(12))) as i16;
    }
}

/// Ports deemphasis_32. x_hi/x_lo/y are L-sample slices.
pub fn deemphasis_32(x_hi: &[i16], x_lo: &[i16], y: &mut [i16], mu: i16, l: i16, mem: &mut i16) {
    let l = l as usize;
    let step = |hi: i16, lo: i16, prev: i16| -> i16 {
        let mut t: i32 = (hi as i32).wrapping_shl(16);
        t = t.wrapping_add((lo as i32).wrapping_shl(4));
        t = shl_int32(t, 3);
        t = fxp_mac_16by16(prev, mu, t);
        t = shl_int32(t, 1);
        amr_wb_round(t)
    };
    y[0] = step(x_hi[0], x_lo[0], *mem);
    for i in 1..l {
        y[i] = step(x_hi[i], x_lo[i], y[i - 1]);
    }
    *mem = y[l - 1];
}

/// Ports highpass_50Hz_at_12k8 (in place). mem has 6 entries.
pub fn highpass_50hz_at_12k8(signal: &mut [i16], lg: i16, mem: &mut [i16]) {
    let (mut y2_hi, mut y2_lo, mut y1_hi, mut y1_lo, mut x0, mut x1) =
        (mem[0], mem[1], mem[2], mem[3], mem[4], mem[5]);
    for i in 0..lg as usize {
        let mut l1 = fxp_mac_16by16(y1_lo, 16211, 8192);
        l1 = fxp_mac_16by16(y2_lo, -8021, l1);
        let mut l2 = fxp_mul_16by16(y1_hi, 32422);
        l2 = fxp_mac_16by16(y2_hi, -16042, l2);
        let x2 = x1;
        x1 = x0;
        x0 = signal[i];
        l2 = fxp_mac_16by16(x2, 8106, l2);
        l2 = fxp_mac_16by16(x1, -16212, l2);
        l2 = fxp_mac_16by16(x0, 8106, l2);
        l1 = ((l1 >> 14).wrapping_add(l2)).wrapping_shl(2);
        y2_hi = y1_hi;
        y2_lo = y1_lo;
        y1_hi = (l1 >> 16) as i16;
        y1_lo = (l1.wrapping_sub((y1_hi as i32).wrapping_shl(16)) >> 1) as i16;
        signal[i] = amr_wb_shl1_round(l1);
    }
    mem[0] = y2_hi;
    mem[1] = y2_lo;
    mem[2] = y1_hi;
    mem[3] = y1_lo;
    mem[4] = x0;
    mem[5] = x1;
}

/// Ports highpass_400Hz_at_12k8 (in place). mem has 6 entries.
pub fn highpass_400hz_at_12k8(signal: &mut [i16], lg: i16, mem: &mut [i16]) {
    let (mut y2_hi, mut y2_lo, mut y1_hi, mut y1_lo, mut x0, mut x1) =
        (mem[0], mem[1], mem[2], mem[3], mem[4], mem[5]);
    for i in 0..lg as usize {
        let mut l1 = fxp_mac_16by16(y1_lo, 29280, 8192);
        let mut l2 = fxp_mul_16by16(y1_hi, 29280);
        l1 = fxp_mac_16by16(y2_lo, -14160, l1);
        l2 = fxp_mac_16by16(y2_hi, -14160, l2);
        let x2 = x1;
        x1 = x0;
        x0 = signal[i];
        l2 = fxp_mac_16by16(x2, 915, l2);
        l2 = fxp_mac_16by16(x1, -1830, l2);
        l2 = fxp_mac_16by16(x0, 915, l2);
        l1 = (l1 >> 13).wrapping_add(l2.wrapping_shl(2));
        y2_hi = y1_hi;
        y2_lo = y1_lo;
        signal[i] = (l1.wrapping_add(0x8000) >> 16) as i16;
        y1_hi = (l1 >> 16) as i16;
        y1_lo = (l1.wrapping_sub((y1_hi as i32).wrapping_shl(16)) >> 1) as i16;
    }
    mem[0] = y2_hi;
    mem[1] = y2_lo;
    mem[2] = y1_hi;
    mem[3] = y1_lo;
    mem[4] = x0;
    mem[5] = x1;
}

/// Ports AmrWbInterpol: `x[base]` is the C `x` pointer.
fn amr_wb_interpol(x: &[i16], base: usize, fir: &[i16; 24]) -> i16 {
    let start = base - 11;
    let mut l: i32 = 0x2000;
    for t in 0..24 {
        l = fxp_mac_16by16(x[start + t], fir[t], l);
    }
    l = shl_int32(l, 2);
    (l >> 16) as i16
}

/// Ports AmrWbUp_samp: `sig_d[base]` is the C `sig_d` pointer (base = NB_COEF_UP in oversamp).
fn amr_wb_up_samp(sig_d: &[i16], base: usize, sig_u: &mut [i16], l_frame: i16) {
    let mut frac: i16 = 1;
    for j in 0..l_frame as i32 {
        let i = ((j * 6554) >> 13) as usize;
        frac -= 1;
        if frac != 0 {
            sig_u[j as usize] = amr_wb_interpol(sig_d, base + i, &FIR_UP[(5 - 1 - frac) as usize]);
        } else {
            sig_u[j as usize] = sig_d[base + i];
            frac = 5;
        }
    }
}

/// Ports oversamp_12k8_to_16k. mem has 24 entries; sig16k gets lg + lg/4 samples.
pub fn oversamp_12k8_to_16k(sig12k8: &[i16], lg: i16, sig16k: &mut [i16], mem: &mut [i16]) {
    let lgu = lg as usize;
    let mut signal = vec![0i16; 24 + lgu];
    signal[..24].copy_from_slice(&mem[..24]);
    signal[24..24 + lgu].copy_from_slice(&sig12k8[..lgu]);
    let lg_up = lg + (lg >> 2);
    amr_wb_up_samp(&signal, 12, sig16k, lg_up);
    mem[..24].copy_from_slice(&signal[lgu..lgu + 24]);
}

/// Ports band_pass_6k_7k (in place on `signal`, 80 samples typ.). mem has 30 entries.
pub fn band_pass_6k_7k(signal: &mut [i16], lg: i16, mem: &mut [i16]) {
    let lg = lg as usize;
    let mut x = vec![0i16; L_FIR_BP + lg];
    x[..L_FIR_BP].copy_from_slice(&mem[..L_FIR_BP]);
    for n in 0..lg {
        x[L_FIR_BP + n] = signal[n] >> 2;
    }
    for n in 0..lg {
        let mut l: i32 = 0x4000;
        l = l.wrapping_sub((x[n + L_FIR_BP] as i32).wrapping_shl(5));
        l = l.wrapping_sub((x[n] as i32).wrapping_shl(5));
        for j in 1..L_FIR_BP {
            l = fxp_mac_16by16(x[n + j], FIR_6K_7K[j], l);
        }
        signal[n] = (l >> 15) as i16;
    }
    mem[..L_FIR_BP].copy_from_slice(&x[lg..lg + L_FIR_BP]);
}

/// Ports low_pass_filt_7k (in place on `signal`). mem has 30 entries.
pub fn low_pass_filt_7k(signal: &mut [i16], lg: i16, mem: &mut [i16]) {
    let lg = lg as usize;
    let mut x = vec![0i16; L_FIR_LP + lg];
    x[..L_FIR_LP].copy_from_slice(&mem[..L_FIR_LP]);
    for n in 0..lg {
        x[L_FIR_LP + n] = signal[n];
    }
    for n in 0..lg {
        let s = (x[n] as i32 + signal[n] as i32) as i16;
        let mut l = fxp_mac_16by16(s, FIR_7K[0], 0x4000);
        for j in 1..L_FIR_LP {
            l = fxp_mac_16by16(x[n + j], FIR_7K[j], l);
        }
        signal[n] = (l >> 15) as i16;
    }
    mem[..L_FIR_LP].copy_from_slice(&x[lg..lg + L_FIR_LP]);
}

#[inline]
fn shl_trunc(v: i16, s: i16) -> i16 {
    ((v as i32).wrapping_shl((s as u32) & 31)) as i16
}

/// Ports isf_extrapolation. `hf_isf` must have at least M16K (20) entries; first M are input.
pub fn isf_extrapolation(hf_isf: &mut [i16]) {
    let mut isf_diff = [0i16; M - 2];
    let mut isf_corr = [0i32; 3];
    hf_isf[M16K - 1] = hf_isf[M - 1];
    for i in 1..(M - 1) {
        isf_diff[i - 1] = sub_int16(hf_isf[i], hf_isf[i - 1]);
    }
    let mut l_tmp: i32 = 0;
    for i in 3..(M - 1) {
        l_tmp = mac_16by16_to_int32(l_tmp, isf_diff[i - 1], 2731);
    }
    let mut mean = amr_wb_round(l_tmp);
    let mut tmp: i16 = 0;
    for i in 0..(M - 2) {
        if isf_diff[i] > tmp {
            tmp = isf_diff[i];
        }
    }
    let exp = norm_s(tmp);
    for i in 0..(M - 2) {
        isf_diff[i] = shl_int16(isf_diff[i], exp);
    }
    mean = shl_int16(mean, exp);
    for (c, lagv) in [(0usize, 2usize), (1, 3), (2, 4)] {
        isf_corr[c] = 0;
        for i in 7..(M - 2) {
            let tmp2 = sub_int16(isf_diff[i], mean);
            let tmp3 = sub_int16(isf_diff[i - lagv], mean);
            let l = mul_16by16_to_int32(tmp2, tmp3);
            let (hi, lo) = super::math::int32_to_dpf(l);
            let l = super::math::mpy_dpf_32(hi, lo, hi, lo);
            isf_corr[c] = add_int32(isf_corr[c], l);
        }
    }
    let mut max_corr: usize = if isf_corr[0] > isf_corr[1] { 0 } else { 1 };
    if isf_corr[2] > isf_corr[max_corr] {
        max_corr = 2;
    }
    max_corr += 1;
    for i in (M - 1)..(M16K - 1) {
        let t = sub_int16(hf_isf[i - 1 - max_corr], hf_isf[i - 2 - max_corr]);
        hf_isf[i] = add_int16(hf_isf[i - 1], t);
    }
    let mut tmp = add_int16(hf_isf[4], hf_isf[3]);
    tmp = sub_int16(hf_isf[2], tmp);
    tmp = mult_int16(tmp, 5461);
    tmp = tmp.wrapping_add(20390);
    if tmp > 19456 {
        tmp = 19456;
    }
    tmp = sub_int16(tmp, hf_isf[M - 2]);
    let mut tmp2 = sub_int16(hf_isf[M16K - 2], hf_isf[M - 2]);
    let exp2 = norm_s(tmp2);
    let mut exp = norm_s(tmp);
    exp = exp.wrapping_sub(1);
    tmp = shl_trunc(tmp, exp);
    tmp2 = shl_trunc(tmp2, exp2);
    let coeff = super::math::div_16by16(tmp, tmp2);
    let exp = exp2.wrapping_sub(exp);
    for i in (M - 1)..(M16K - 1) {
        let t = mult_int16(sub_int16(hf_isf[i], hf_isf[i - 1]), coeff);
        isf_diff[i - (M - 1)] = shl_int16(t, exp);
    }
    for i in M..(M16K - 1) {
        let t = (isf_diff[i - (M - 1)] as i32 + isf_diff[i - M] as i32 - 1280) as i16;
        if t < 0 {
            if isf_diff[i - (M - 1)] > isf_diff[i - M] {
                isf_diff[i - M] = (1280 - isf_diff[i - (M - 1)] as i32) as i16;
            } else {
                isf_diff[i - (M - 1)] = (1280 - isf_diff[i - M] as i32) as i16;
            }
        }
    }
    for i in (M - 1)..(M16K - 1) {
        hf_isf[i] = add_int16(hf_isf[i - 1], isf_diff[i - (M - 1)]);
    }
    for i in 0..(M16K - 1) {
        hf_isf[i] = mult_int16(hf_isf[i], 26214);
    }
    let src: Vec<i16> = hf_isf[..M16K].to_vec();
    super::isf::isf_isp(&src, hf_isf, M16K as i16);
}

/// Ports synthesis_amr_wb for one 64-sample subframe.
/// In: `a` (M+1 coeffs), `q_new`, `prms` (HF gain index), `nb_bits`, `new_dtx_state`, `bfi`.
/// In/out: `exc` (64 samples, scaled in place), `hf_isf` (>=20 entries, modified by
/// isf_extrapolation when nb_bits<=NBBITS_7K and speech), `st`. Out: `synth16k` (80 samples).
pub fn synthesis_amr_wb(
    a: &[i16],
    exc: &mut [i16],
    q_new: i16,
    synth16k: &mut [i16],
    prms: i16,
    hf_isf: &mut [i16],
    nb_bits: i16,
    new_dtx_state: i16,
    st: &mut SynthState,
    bfi: i16,
) {
    let mut q_new = q_new;
    let mut synth_hi = vec![0i16; M + L_SUBFR];
    let mut synth_lo = vec![0i16; M + L_SUBFR];
    let mut synth = vec![0i16; L_SUBFR];
    let mut hf = vec![0i16; L_SUBFR16K];
    let mut ap = vec![0i16; M16K + 1];
    let mut hf_a = vec![0i16; M16K + 1];
    let mut scratch = vec![0i16; 2 * M16K + L_SUBFR16K + 64];

    synth_hi[..M].copy_from_slice(&st.mem_syn_hi);
    synth_lo[..M].copy_from_slice(&st.mem_syn_lo);

    syn_filt_32(
        a,
        M as i16,
        exc,
        q_new,
        &mut synth_hi,
        &mut synth_lo,
        L_SUBFR as i16,
    );

    st.mem_syn_hi
        .copy_from_slice(&synth_hi[L_SUBFR..L_SUBFR + M]);
    st.mem_syn_lo
        .copy_from_slice(&synth_lo[L_SUBFR..L_SUBFR + M]);

    deemphasis_32(
        &synth_hi[M..],
        &synth_lo[M..],
        &mut synth,
        PREEMPH_FAC,
        L_SUBFR as i16,
        &mut st.mem_deemph,
    );

    highpass_50hz_at_12k8(&mut synth, L_SUBFR as i16, &mut st.mem_sig_out);

    oversamp_12k8_to_16k(&synth, L_SUBFR as i16, synth16k, &mut st.mem_oversamp);

    for v in hf.iter_mut() {
        *v = super::exc::noise_gen_amrwb(&mut st.seed2) >> 3;
    }

    for v in exc[..L_SUBFR].iter_mut() {
        *v = add_int16(*v, 0x0004) >> 3;
    }

    q_new = q_new.wrapping_sub(3);
    let mut exp_ener: i16 = 0;
    let mut exp: i16 = 0;
    let mut ener = super::math::extract_h(super::math::dot_product12(
        exc,
        exc,
        L_SUBFR as i16,
        &mut exp_ener,
    ));
    exp_ener = exp_ener.wrapping_sub(q_new.wrapping_shl(1));

    let mut tmp = super::math::extract_h(super::math::dot_product12(
        &hf,
        &hf,
        L_SUBFR16K as i16,
        &mut exp,
    ));
    if tmp > ener {
        tmp >>= 1;
        exp = exp.wrapping_add(1);
    }
    let mut l_tmp = super::math::l_deposit_h(super::math::div_16by16(tmp, ener));
    exp = exp.wrapping_sub(exp_ener);
    super::math::one_ov_sqrt_norm(&mut l_tmp, &mut exp);
    l_tmp = shl_int32(l_tmp, exp.wrapping_add(1));
    tmp = (l_tmp >> 16) as i16;

    for v in hf.iter_mut() {
        *v = fxp_mul_16by16(*v, tmp).wrapping_shr(15) as i16;
    }

    highpass_400hz_at_12k8(&mut synth, L_SUBFR as i16, &mut st.mem_hp400);

    let mut l_tmp: i32 = 1;
    let mut l_tmp2: i32 = 1;
    l_tmp = mac_16by16_to_int32(l_tmp, synth[0], synth[0]);
    for i in 1..L_SUBFR {
        l_tmp = mac_16by16_to_int32(l_tmp, synth[i], synth[i]);
        l_tmp2 = mac_16by16_to_int32(l_tmp2, synth[i], synth[i - 1]);
    }
    exp = normalize_amr_wb(l_tmp);
    ener = (l_tmp.wrapping_shl(exp as u32 & 31) >> 16) as i16;
    tmp = (l_tmp2.wrapping_shl(exp as u32 & 31) >> 16) as i16;
    let fac = if tmp > 0 {
        super::math::div_16by16(tmp, ener)
    } else {
        0
    };

    let gain1: i16 = 32767i16.wrapping_sub(fac);
    let mut gain2 = mult_int16(gain1, 20480);
    gain2 = shl_int16(gain2, 1);
    tmp = if st.vad_hist > 0 {
        gain2.wrapping_sub(1)
    } else {
        gain1.wrapping_sub(1)
    };
    if tmp != 0 {
        tmp = tmp.wrapping_add(1);
    }
    if tmp < 3277 {
        tmp = 3277;
    }

    if nb_bits >= NBBITS_24K && bfi == 0 {
        let hf_corr_gain = HP_GAIN[(prms as usize) & 15];
        for v in hf.iter_mut() {
            *v = mult_int16(*v, hf_corr_gain).wrapping_shl(1);
        }
    } else {
        for v in hf.iter_mut() {
            *v = mult_int16(*v, tmp);
        }
    }

    let hf_in = hf.clone();
    if nb_bits <= NBBITS_7K && new_dtx_state == SPEECH {
        isf_extrapolation(hf_isf);
        super::isf::isp_az(hf_isf, &mut hf_a, M16K as i16, 0);
        super::isf::weight_amrwb_lpc(&hf_a, &mut ap, 29491, M16K as i16);
        wb_syn_filt(
            &ap,
            M16K as i16,
            &hf_in,
            &mut hf,
            L_SUBFR16K as i16,
            &mut st.mem_syn_hf,
            1,
            &mut scratch,
        );
    } else {
        super::isf::weight_amrwb_lpc(a, &mut ap, 19661, M as i16);
        wb_syn_filt(
            &ap,
            M as i16,
            &hf_in,
            &mut hf,
            L_SUBFR16K as i16,
            &mut st.mem_syn_hf[M16K - M..],
            1,
            &mut scratch,
        );
    }

    band_pass_6k_7k(&mut hf, L_SUBFR16K as i16, &mut st.mem_hf);

    if nb_bits >= NBBITS_24K {
        low_pass_filt_7k(&mut hf, L_SUBFR16K as i16, &mut st.mem_hf3);
    }

    for i in 0..L_SUBFR16K {
        synth16k[i] = add_int16(synth16k[i], hf[i]);
    }
}

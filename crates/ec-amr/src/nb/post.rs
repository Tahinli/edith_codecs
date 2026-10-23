//! AMR-NB post-processing chain: syn_filt, residu, weight_ai, a_refl, preemphasis,
//! agc, post_filter, post_process (bit-exact port of opencore-amr).
#![allow(clippy::all)]
#![allow(dead_code)]

use super::basic::*;
use super::tables::Mode;

const M: usize = 10;
const MP1: usize = 11;
const L_SUBFR: usize = 40;
const L_FRAME: usize = 160;
const L_H: usize = 22;
const MU: i32 = 26214;
const AGC_FAC: i16 = 29491;

static GAMMA3_MR122: [i16; M] = [22938, 16057, 11240, 7868, 5508, 3856, 2699, 1889, 1322, 925];
static GAMMA3: [i16; M] = [18022, 9912, 5451, 2998, 1649, 907, 499, 274, 151, 83];
static GAMMA4_MR122: [i16; M] = [24576, 18432, 13824, 10368, 7776, 5832, 4374, 3281, 2461, 1846];
static GAMMA4: [i16; M] = [22938, 16057, 11240, 7868, 5508, 3856, 2699, 1889, 1322, 925];

#[inline]
fn sat12(s: i32) -> i16 {
    if (s.wrapping_add(134217728) as u32) < 0x0fff_ffff {
        (s >> 12) as i16
    } else if s > 0x07ff_ffff {
        32767
    } else {
        -32768
    }
}

/// C Syn_filt: y[i] = x[i] - sum a[j]*y[i-j] (a has M+1 coefs, lg <= 80).
/// x and y are separate slices (len >= lg); mem is the M-sample filter memory,
/// updated with the last M outputs when `update` is true.
pub fn syn_filt(a: &[i16], x: &[i16], y: &mut [i16], lg: usize, mem: &mut [i16], update: bool) {
    let mut tmp = [0i16; M + 80];
    tmp[..M].copy_from_slice(&mem[..M]);
    for i in 0..lg {
        let mut s: i32 = (x[i] as i32).wrapping_mul(a[0] as i32).wrapping_add(0x800);
        for j in 1..=M {
            s = s.wrapping_sub((a[j] as i32).wrapping_mul(tmp[M + i - j] as i32));
        }
        let v = sat12(s);
        tmp[M + i] = v;
        y[i] = v;
    }
    if update {
        mem[..M].copy_from_slice(&y[lg - M..lg]);
    }
}

/// C Residu: y[i] = sum_{j=0..M} a[j]*x[xoff+i-j] (>>12, truncating). x[xoff-M..xoff]
/// holds the history; y is a separate slice of length >= lg.
pub fn residu(a: &[i16], x: &[i16], xoff: usize, y: &mut [i16], lg: usize) {
    for i in 0..lg {
        let mut s: i32 = 0x800;
        for j in 0..=M {
            s = s.wrapping_add((a[j] as i32).wrapping_mul(x[xoff + i - j] as i32));
        }
        y[i] = (s >> 12) as i16;
    }
}

/// C Weight_Ai: a_exp[0]=a[0]; a_exp[i] = (a[i]*fac[i-1] + 0x4000) >> 15, i=1..=M.
pub fn weight_ai(a: &[i16], fac: &[i16], a_exp: &mut [i16]) {
    a_exp[0] = a[0];
    for i in 1..=M {
        a_exp[i] = (((a[i] as i32) * (fac[i - 1] as i32) + 0x4000) >> 15) as i16;
    }
}

/// C A_Refl: direct form a[0..M] (note: M coefficients, without a[0]=1 term) to
/// reflection coefficients refl[0..M]. Reproduces opencore's control flow.
pub fn a_refl(a: &[i16], refl: &mut [i16]) {
    let mut a_state = [0i16; M];
    let mut b_state = [0i16; M];
    a_state.copy_from_slice(&a[..M]);
    let mut i = M as i32 - 1;
    while i >= 0 {
        let iu = i as usize;
        if abs_s(a_state[iu]) >= 4096 {
            for k in 0..M {
                refl[k] = 0;
            }
            break;
        }
        refl[iu] = shl(a_state[iu], 3);
        let l_temp = l_mult(refl[iu], refl[iu]);
        let mut l_acc = l_sub(i32::MAX, l_temp);
        let norm_shift = norm_l(l_acc);
        let scale = 15 - norm_shift;
        l_acc = l_shl(l_acc, norm_shift);
        // opencore's pv_round(L_add) loses its saturation check here when built with optimisation
        // (signed-overflow UB), so the reference output wraps for l_acc close to i32::MAX.
        let norm_prod = (l_acc.wrapping_add(0x8000) >> 16) as i16;
        let mult_ = div_s(16384, norm_prod);
        for j in 0..iu {
            let mut l_acc = (a_state[j] as i32).wrapping_shl(16);
            l_acc = l_msu(l_acc, refl[iu], a_state[iu - j - 1]);
            let temp = round_(l_acc);
            let mut l_temp = l_mult(mult_, temp);
            l_temp = l_shr_r(l_temp, scale);
            let t = l_temp.wrapping_sub((l_temp < 0) as i32);
            let t_abs = t ^ (t >> 31);
            if t_abs > 32767 {
                for k in 0..M {
                    refl[k] = 0;
                }
                break;
            }
            b_state[j] = l_temp as i16;
        }
        for j in 0..iu {
            a_state[j] = b_state[j];
        }
        i -= 1;
    }
}

/// C preemphasisState.
#[derive(Clone, Debug, Default)]
pub struct PreemphasisState {
    pub mem_pre: i16,
}

impl PreemphasisState {
    pub fn new() -> Self {
        Self { mem_pre: 0 }
    }
    pub fn reset(&mut self) {
        self.mem_pre = 0;
    }
}

/// C preemphasis: in-place signal[0..l] -= g*signal[i-1] (mem for i=0).
pub fn preemphasis(st: &mut PreemphasisState, signal: &mut [i16], g: i16, l: usize) {
    let temp = signal[l - 1];
    for i in (1..l).rev() {
        let t2 = mult(g, signal[i - 1]);
        signal[i] = sub(signal[i], t2);
    }
    let t2 = mult(g, st.mem_pre);
    signal[0] = sub(signal[0], t2);
    st.mem_pre = temp;
}

/// C agcState.
#[derive(Clone, Debug)]
pub struct AgcState {
    pub past_gain: i16,
}

impl AgcState {
    pub fn new() -> Self {
        Self { past_gain: 4096 }
    }
    pub fn reset(&mut self) {
        self.past_gain = 4096;
    }
}

fn energy_new(inp: &[i16], l: usize) -> i32 {
    let mut s: i32 = 0;
    for i in 0..l {
        s = l_mac(s, inp[i], inp[i]);
    }
    if s != i32::MAX {
        s >>= 4;
    } else {
        // C energy_old
        s = 0;
        for &x in &inp[..l] {
            let t = x >> 2;
            s = l_mac(s, t, t);
        }
    }
    s
}

/// C agc: scale sig_out (in place, len l) to the energy of sig_in.
pub fn agc(st: &mut AgcState, sig_in: &[i16], sig_out: &mut [i16], agc_fac: i16, l: usize) {
    let mut s = energy_new(&sig_out[..l], l);
    if s == 0 {
        st.past_gain = 0;
        return;
    }
    let mut exp = norm_l(s) - 1;
    let l_temp = l_shl(s, exp);
    let gain_out = round_(l_temp);
    s = energy_new(&sig_in[..l], l);
    let g0: i16;
    if s == 0 {
        g0 = 0;
    } else {
        let i = norm_l(s);
        let l_temp = s.wrapping_shl(i as u32);
        let gain_in = round_(l_temp);
        exp -= i;
        let temp = div_s(gain_out, gain_in);
        s = (temp as i32).wrapping_shl(7);
        s = l_shr(s, exp);
        s = inv_sqrt(s);
        let l_temp = s.wrapping_shl(9);
        let i = (l_temp.wrapping_add(0x8000) >> 16) as i16;
        let temp = 32767 - agc_fac;
        g0 = (((i as i32) * (temp as i32)) >> 15) as i16;
    }
    let mut gain = st.past_gain;
    for i in 0..l {
        gain = (((gain as i32) * (agc_fac as i32)) >> 15) as i16;
        gain = gain.wrapping_add(g0);
        let l_temp = ((sig_out[i] as i32) * (gain as i32)).wrapping_shl(1);
        sig_out[i] = (l_temp >> 13) as i16;
    }
    st.past_gain = gain;
}

/// C Post_FilterState. synth_buf[0..M] is history, current frame at synth_buf[M..].
#[derive(Clone, Debug)]
pub struct PostFilterState {
    pub res2: [i16; L_SUBFR],
    pub mem_syn_pst: [i16; M],
    pub preemph_state: PreemphasisState,
    pub agc_state: AgcState,
    pub synth_buf: [i16; M + L_FRAME],
}

impl PostFilterState {
    pub fn new() -> Self {
        Self {
            res2: [0; L_SUBFR],
            mem_syn_pst: [0; M],
            preemph_state: PreemphasisState::new(),
            agc_state: AgcState::new(),
            synth_buf: [0; M + L_FRAME],
        }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// C Post_Filter: syn[0..160] is the synthesis speech, replaced by the postfiltered
/// output; az_4 holds 4 subframes of MP1(11) LPC coefs each (44 values).
pub fn post_filter(st: &mut PostFilterState, mode: Mode, syn: &mut [i16], az_4: &[i16]) {
    st.synth_buf[M..M + L_FRAME].copy_from_slice(&syn[..L_FRAME]);
    let mut ap3 = [0i16; MP1];
    let mut ap4 = [0i16; MP1];
    let mut i_subfr = 0;
    let mut az_off = 0;
    while i_subfr < L_FRAME {
        let az = &az_4[az_off..az_off + MP1];
        if mode == Mode::Mr122 || mode == Mode::Mr102 {
            weight_ai(az, &GAMMA3_MR122, &mut ap3);
            weight_ai(az, &GAMMA4_MR122, &mut ap4);
        } else {
            weight_ai(az, &GAMMA3, &mut ap3);
            weight_ai(az, &GAMMA4, &mut ap4);
        }
        residu(&ap3, &st.synth_buf, M + i_subfr, &mut st.res2, L_SUBFR);

        // impulse response of A(z/0.7)/A(z/0.75)
        let mut h = [0i16; L_H];
        h[..MP1].copy_from_slice(&ap3);
        let hx = h;
        let mut zmem = [0i16; M];
        syn_filt(&ap4, &hx, &mut h, L_H, &mut zmem, false);

        // 1st correlation
        let mut l_tmp: i32 = 0;
        for i in (0..L_H).rev() {
            let l2 = (h[i] as i32) * (h[i] as i32);
            if l2 != 0x4000_0000 {
                l_tmp = l_add(l_tmp, l2 << 1);
            } else {
                break;
            }
        }
        let temp1 = (l_tmp >> 16) as i16;
        l_tmp = 0;
        for i in (0..L_H - 1).rev() {
            let l2 = (h[i] as i32) * (h[i + 1] as i32);
            if l2 != 0x4000_0000 {
                l_tmp = l_add(l_tmp, l2 << 1);
            } else {
                break;
            }
        }
        let mut temp2 = (l_tmp >> 16) as i16;
        if temp2 <= 0 {
            temp2 = 0;
        } else {
            let mut t = ((temp2 as i32) * MU) >> 15;
            if t & 0x0001_0000 != 0 {
                t |= 0xffff_0000u32 as i32;
            }
            temp2 = t as i16;
            temp2 = div_s(temp2, temp1);
        }
        preemphasis(&mut st.preemph_state, &mut st.res2, temp2, L_SUBFR);

        let res2 = st.res2;
        syn_filt(
            &ap4,
            &res2,
            &mut syn[i_subfr..i_subfr + L_SUBFR],
            L_SUBFR,
            &mut st.mem_syn_pst,
            true,
        );
        agc(
            &mut st.agc_state,
            &st.synth_buf[M + i_subfr..M + i_subfr + L_SUBFR],
            &mut syn[i_subfr..i_subfr + L_SUBFR],
            AGC_FAC,
            L_SUBFR,
        );
        az_off += MP1;
        i_subfr += L_SUBFR;
    }
    st.synth_buf.copy_within(L_FRAME..L_FRAME + M, 0);
}

/// C Post_ProcessState (2nd order high-pass + x2 upscaling).
#[derive(Clone, Debug, Default)]
pub struct PostProcessState {
    pub y2_hi: i16,
    pub y2_lo: i16,
    pub y1_hi: i16,
    pub y1_lo: i16,
    pub x0: i16,
    pub x1: i16,
}

impl PostProcessState {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// C Post_Process: in-place high-pass filter and upscale of signal[0..lg].
pub fn post_process(st: &mut PostProcessState, signal: &mut [i16], lg: usize) {
    const C_A1: i32 = 15836;
    const C_A2: i32 = -7667;
    const C_B0: i32 = 7699;
    const C_B1: i32 = -15398;
    const C_B2: i32 = 7699;
    for p in signal[..lg].iter_mut() {
        let x2 = st.x1;
        st.x1 = st.x0;
        st.x0 = *p;
        let mut l_tmp: i32 = (st.y1_hi as i32).wrapping_mul(C_A1);
        l_tmp = l_tmp.wrapping_add(((st.y1_lo as i32).wrapping_mul(C_A1)) >> 15);
        l_tmp = l_tmp.wrapping_add((st.y2_hi as i32).wrapping_mul(C_A2));
        l_tmp = l_tmp.wrapping_add(((st.y2_lo as i32).wrapping_mul(C_A2)) >> 15);
        l_tmp = l_tmp.wrapping_add((st.x0 as i32).wrapping_mul(C_B0));
        l_tmp = l_tmp.wrapping_add((st.x1 as i32).wrapping_mul(C_B1));
        l_tmp = l_tmp.wrapping_add((x2 as i32).wrapping_mul(C_B2));
        l_tmp = l_shl(l_tmp, 3);
        *p = round_(l_shl(l_tmp, 1));
        st.y2_hi = st.y1_hi;
        st.y2_lo = st.y1_lo;
        st.y1_hi = (l_tmp >> 16) as i16;
        st.y1_lo = ((l_tmp >> 1).wrapping_sub((st.y1_hi as i32) << 15)) as i16;
    }
}

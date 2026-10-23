//! LSF/LSP decoding, conversion and interpolation (d_plsf*, int_lsf, lsp_lsf,
//! lsp_az, int_lpc, reorder). Bit-exact port of the 3GPP reference.
#![allow(dead_code)]

use super::basic::{add, mult, negate, shl, sub};
use super::tables::{
    Mode, DICO1_LSF_3, DICO1_LSF_5, DICO2_LSF_3, DICO2_LSF_5, DICO3_LSF_3, DICO3_LSF_5,
    DICO4_LSF_5, DICO5_LSF_5, MEAN_LSF_3, MEAN_LSF_5, MR515_3_LSF, MR795_1_LSF, PAST_RQ_INIT,
    PRED_FAC_3, SLOPE, TABLE,
};

const M: usize = 10;
const MP1: usize = 11;
const LSF_GAP: i16 = 205;
const LSP_PRED_FAC_MR122: i32 = 21299;
const DICO1_SIZE: i16 = 256;
const DICO2_SIZE: i16 = 512;
const DICO3_SIZE: i16 = 512;
const MR515_3_SIZE: i16 = 128;
const MR795_1_SIZE: i16 = 512;

/// State of the LSF decoder (C `D_plsfState`), shared by D_plsf_3 and D_plsf_5.
#[derive(Clone, Debug)]
pub struct DPlsfState {
    pub past_r_q: [i16; M],
    pub past_lsf_q: [i16; M],
}

impl DPlsfState {
    /// C `D_plsf_reset`: past_r_q = 0, past_lsf_q = mean_lsf_5.
    pub fn new() -> Self {
        let mut s = DPlsfState { past_r_q: [0; M], past_lsf_q: [0; M] };
        s.reset();
        s
    }
    pub fn reset(&mut self) {
        self.past_r_q = [0; M];
        self.past_lsf_q.copy_from_slice(&MEAN_LSF_5[..M]);
    }
}

impl Default for DPlsfState {
    fn default() -> Self {
        Self::new()
    }
}

/// C `Init_D_plsf_3`: past_r_q = past_rq_init[index*M ..]; `index` is parm[0] of a SID frame.
pub fn init_d_plsf_3(st: &mut DPlsfState, index: i16) {
    let o = (index as usize) * M;
    st.past_r_q.copy_from_slice(&PAST_RQ_INIT[o..o + M]);
}

/// C `Reorder_lsf`: enforce minimum distance `min_dist` over the first `n` entries.
pub fn reorder_lsf(lsf: &mut [i16], min_dist: i16, n: usize) {
    let mut lsf_min = min_dist;
    for v in lsf[..n].iter_mut() {
        if *v < lsf_min {
            *v = lsf_min;
            lsf_min = lsf_min.wrapping_add(min_dist);
        } else {
            lsf_min = v.wrapping_add(min_dist);
        }
    }
}

/// C `Lsf_lsp`: convert first `m` LSFs (0..0.5 Q15 freq) to LSPs (cosine domain).
pub fn lsf_lsp(lsf: &[i16], lsp: &mut [i16], m: usize) {
    for i in 0..m {
        let ind = ((lsf[i] >> 8) as i32).clamp(0, 63) as usize;
        let offset = (lsf[i] & 0x00ff) as i32;
        let l_tmp = ((TABLE[ind + 1] as i32 - TABLE[ind] as i32) * offset) >> 8;
        lsp[i] = TABLE[ind].wrapping_add(l_tmp as i16);
    }
}

/// C `Lsp_lsf`: convert first `m` LSPs to LSFs.
pub fn lsp_lsf(lsp: &[i16], lsf: &mut [i16], m: usize) {
    let mut ind: usize = 63;
    for i in (0..m).rev() {
        let temp = lsp[i];
        while ind > 0 && TABLE[ind] < temp {
            ind -= 1;
        }
        let mut l_tmp = (temp as i32 - TABLE[ind] as i32).wrapping_mul(SLOPE[ind] as i32);
        l_tmp = l_tmp.wrapping_add(0x800) >> 12;
        lsf[i] = (l_tmp as i16).wrapping_add((ind as i32 as i16) << 8);
    }
}

#[inline]
fn m16(a: i16, b: i32) -> i16 {
    ((a as i32 * b) >> 15) as i16
}

/// C `D_plsf_3`: decode 3 LSF indices (`indice[0..3]`) into one LSP set `lsp1_q[0..10]`.
/// `bfi != 0` means bad frame (indices unused).
pub fn d_plsf_3(st: &mut DPlsfState, mode: Mode, bfi: i16, indice: &[i16], lsp1_q: &mut [i16]) {
    let mut lsf1_q = [0i16; M];
    if bfi != 0 {
        for i in 0..M {
            let temp = mult(st.past_lsf_q[i], 29491);
            let index = mult(MEAN_LSF_3[i], 3277);
            lsf1_q[i] = add(index, temp);
        }
        if mode != Mode::MrDtx {
            for i in 0..M {
                let t = mult(st.past_r_q[i], PRED_FAC_3[i]);
                let t = add(MEAN_LSF_3[i], t);
                st.past_r_q[i] = sub(lsf1_q[i], t);
            }
        } else {
            for i in 0..M {
                let t = add(MEAN_LSF_3[i], st.past_r_q[i]);
                st.past_r_q[i] = sub(lsf1_q[i], t);
            }
        }
    } else {
        let mut lsf1_r = [0i16; M];
        let index_limit_2 = (DICO2_SIZE - 1) * 3;
        let (p_cb1, p_cb3, index_limit_1, index_limit_3): (&[i16], &[i16], i16, i16) =
            if mode == Mode::Mr475 || mode == Mode::Mr515 {
                (&DICO1_LSF_3[..], &MR515_3_LSF[..], (DICO1_SIZE - 1) * 3, (MR515_3_SIZE - 1) * 4)
            } else if mode == Mode::Mr795 {
                (&MR795_1_LSF[..], &DICO3_LSF_3[..], (MR795_1_SIZE - 1) * 3, (DICO3_SIZE - 1) * 4)
            } else {
                (&DICO1_LSF_3[..], &DICO3_LSF_3[..], (DICO1_SIZE - 1) * 3, (DICO3_SIZE - 1) * 4)
            };

        let index = indice[0];
        let mut temp = index.wrapping_add(index << 1);
        if temp > index_limit_1 {
            temp = index_limit_1;
        }
        let t = temp.max(0) as usize;
        lsf1_r[0..3].copy_from_slice(&p_cb1[t..t + 3]);

        let mut index = indice[1];
        if mode == Mode::Mr475 || mode == Mode::Mr515 {
            index <<= 1;
        }
        let mut temp = index.wrapping_add(index << 1);
        if temp > index_limit_2 {
            temp = index_limit_2;
        }
        let t = temp.max(0) as usize;
        lsf1_r[3..6].copy_from_slice(&DICO2_LSF_3[t..t + 3]);

        let index = indice[2];
        let mut temp = index << 2;
        if temp > index_limit_3 {
            temp = index_limit_3;
        }
        let t = temp.max(0) as usize;
        lsf1_r[6..10].copy_from_slice(&p_cb3[t..t + 4]);

        if mode != Mode::MrDtx {
            for i in 0..M {
                let t = mult(st.past_r_q[i], PRED_FAC_3[i]);
                let t = add(MEAN_LSF_3[i], t);
                lsf1_q[i] = add(lsf1_r[i], t);
                st.past_r_q[i] = lsf1_r[i];
            }
        } else {
            for i in 0..M {
                let t = add(MEAN_LSF_3[i], st.past_r_q[i]);
                lsf1_q[i] = add(lsf1_r[i], t);
                st.past_r_q[i] = lsf1_r[i];
            }
        }
    }
    reorder_lsf(&mut lsf1_q, LSF_GAP, M);
    st.past_lsf_q = lsf1_q;
    lsf_lsp(&lsf1_q, lsp1_q, M);
}

/// C `D_plsf_5`: decode 5 LSF indices (`indice[0..5]`) into two LSP sets
/// `lsp1_q[0..10]`, `lsp2_q[0..10]` (MR122).
pub fn d_plsf_5(
    st: &mut DPlsfState,
    bfi: i16,
    indice: &[i16],
    lsp1_q: &mut [i16],
    lsp2_q: &mut [i16],
) {
    let mut lsf1_q = [0i16; M];
    let mut lsf2_q = [0i16; M];
    if bfi != 0 {
        for i in 0..M {
            let temp = m16(st.past_lsf_q[i], 31128);
            let sign = m16(MEAN_LSF_5[i], 1639);
            lsf1_q[i] = add(sign, temp);
            lsf2_q[i] = lsf1_q[i];
            let temp = m16(st.past_r_q[i], LSP_PRED_FAC_MR122);
            let temp = add(MEAN_LSF_5[i], temp);
            st.past_r_q[i] = sub(lsf2_q[i], temp);
        }
    } else {
        let mut lsf1_r = [0i16; M];
        let mut lsf2_r = [0i16; M];
        let fetch = |dico: &[i16], idx: i16, r1: &mut [i16; M], r2: &mut [i16; M], o: usize, neg: bool| {
            let t = (shl(idx, 2).max(0)) as usize;
            let p = &dico[t..t + 4];
            let f = |v: i16| if neg { negate(v) } else { v };
            r1[o] = f(p[0]);
            r1[o + 1] = f(p[1]);
            r2[o] = f(p[2]);
            r2[o + 1] = f(p[3]);
        };
        fetch(&DICO1_LSF_5, indice[0], &mut lsf1_r, &mut lsf2_r, 0, false);
        fetch(&DICO2_LSF_5, indice[1], &mut lsf1_r, &mut lsf2_r, 2, false);
        let sign = indice[2] & 1;
        fetch(&DICO3_LSF_5, indice[2] >> 1, &mut lsf1_r, &mut lsf2_r, 4, sign != 0);
        fetch(&DICO4_LSF_5, indice[3], &mut lsf1_r, &mut lsf2_r, 6, false);
        fetch(&DICO5_LSF_5, indice[4], &mut lsf1_r, &mut lsf2_r, 8, false);

        for i in 0..M {
            let temp = mult(st.past_r_q[i], LSP_PRED_FAC_MR122 as i16);
            let temp = add(MEAN_LSF_5[i], temp);
            lsf1_q[i] = add(lsf1_r[i], temp);
            lsf2_q[i] = add(lsf2_r[i], temp);
            st.past_r_q[i] = lsf2_r[i];
        }
    }
    reorder_lsf(&mut lsf1_q, LSF_GAP, M);
    reorder_lsf(&mut lsf2_q, LSF_GAP, M);
    st.past_lsf_q = lsf2_q;
    lsf_lsp(&lsf1_q, lsp1_q, M);
    lsf_lsp(&lsf2_q, lsp2_q, M);
}

/// C `Int_lsf`: interpolate LSFs for subframe start `i_subfr` (0, 40, 80, 120);
/// other values leave `lsf_out` untouched.
pub fn int_lsf(lsf_old: &[i16], lsf_new: &[i16], i_subfr: i16, lsf_out: &mut [i16]) {
    for i in (0..M).rev() {
        let (o, n) = (lsf_old[i], lsf_new[i]);
        match i_subfr {
            0 => lsf_out[i] = add(o.wrapping_sub(o >> 2), n >> 2),
            40 => lsf_out[i] = (o >> 1).wrapping_add(n >> 1),
            80 => lsf_out[i] = add(o >> 2, n.wrapping_sub(n >> 2)),
            120 => lsf_out[i] = n,
            _ => {}
        }
    }
}

fn get_lsp_pol(lsp: &[i16], f: &mut [i32; 6]) {
    f[0] = 0x0100_0000;
    f[1] = (-(lsp[0] as i32)).wrapping_shl(10);
    let mut l = 2usize;
    let mut k = 2usize;
    for i in 2..=5usize {
        f[k] = f[k - 2];
        for _ in 1..i {
            let hi = (f[k - 1] >> 16) as i16;
            let lo = ((f[k - 1] >> 1).wrapping_sub((hi as i32) << 15)) as i16;
            let mut t0 = (hi as i32).wrapping_mul(lsp[l] as i32);
            t0 = t0.wrapping_add(((lo as i32).wrapping_mul(lsp[l] as i32)) >> 15);
            f[k] = f[k].wrapping_add(f[k - 2]);
            f[k] = f[k].wrapping_sub(t0.wrapping_shl(2));
            k -= 1;
        }
        f[k] = f[k].wrapping_sub((lsp[l] as i32).wrapping_shl(10));
        k += i;
        l += 2;
    }
}

/// C `Lsp_Az`: LSPs `lsp[0..10]` to LP coefficients `a[0..11]` (Q12).
pub fn lsp_az(lsp: &[i16], a: &mut [i16]) {
    let mut f1 = [0i32; 6];
    let mut f2 = [0i32; 6];
    get_lsp_pol(&lsp[0..], &mut f1);
    get_lsp_pol(&lsp[1..], &mut f2);
    for i in (1..=5).rev() {
        f1[i] = f1[i].wrapping_add(f1[i - 1]);
        f2[i] = f2[i].wrapping_sub(f2[i - 1]);
    }
    a[0] = 4096;
    let mut j = 10usize;
    for i in 1..=5usize {
        let t0 = f1[i].wrapping_add(f2[i]).wrapping_add(1 << 12);
        let t1 = f1[i].wrapping_sub(f2[i]).wrapping_add(1 << 12);
        a[i] = (t0 >> 13) as i16;
        a[j] = (t1 >> 13) as i16;
        j -= 1;
    }
}

#[inline]
fn half_sum(a: &[i16], b: &[i16], out: &mut [i16; M]) {
    for i in 0..M {
        out[i] = (a[i] >> 1).wrapping_add(b[i] >> 1);
    }
}

/// C `Int_lpc_1and3`: 4 subframes; writes Az[0..44] (4 sets of MP1=11 coeffs).
pub fn int_lpc_1and3(lsp_old: &[i16], lsp_mid: &[i16], lsp_new: &[i16], az: &mut [i16]) {
    let mut lsp = [0i16; M];
    half_sum(lsp_old, lsp_mid, &mut lsp);
    lsp_az(&lsp, &mut az[0..]);
    lsp_az(lsp_mid, &mut az[MP1..]);
    half_sum(lsp_mid, lsp_new, &mut lsp);
    lsp_az(&lsp, &mut az[2 * MP1..]);
    lsp_az(lsp_new, &mut az[3 * MP1..]);
}

/// C `Int_lpc_1and3_2`: writes Az[0..11] (subframe 1) and Az[22..33] (subframe 3).
pub fn int_lpc_1and3_2(lsp_old: &[i16], lsp_mid: &[i16], lsp_new: &[i16], az: &mut [i16]) {
    let mut lsp = [0i16; M];
    half_sum(lsp_old, lsp_mid, &mut lsp);
    lsp_az(&lsp, &mut az[0..]);
    half_sum(lsp_mid, lsp_new, &mut lsp);
    lsp_az(&lsp, &mut az[2 * MP1..]);
}

/// C `Int_lpc_1to3`: 4 subframes from old/new LSPs; writes Az[0..44].
pub fn int_lpc_1to3(lsp_old: &[i16], lsp_new: &[i16], az: &mut [i16]) {
    let mut lsp = [0i16; M];
    for i in 0..M {
        lsp[i] = (lsp_old[i].wrapping_sub(lsp_old[i] >> 2)).wrapping_add(lsp_new[i] >> 2);
    }
    lsp_az(&lsp, &mut az[0..]);
    half_sum(lsp_new, lsp_old, &mut lsp);
    lsp_az(&lsp, &mut az[MP1..]);
    for i in 0..M {
        lsp[i] = (lsp_new[i].wrapping_sub(lsp_new[i] >> 2)).wrapping_add(lsp_old[i] >> 2);
    }
    lsp_az(&lsp, &mut az[2 * MP1..]);
    lsp_az(lsp_new, &mut az[3 * MP1..]);
}

/// C `Int_lpc_1to3_2`: like `int_lpc_1to3` but only Az[0..33] (first 3 subframes).
pub fn int_lpc_1to3_2(lsp_old: &[i16], lsp_new: &[i16], az: &mut [i16]) {
    let mut lsp = [0i16; M];
    for i in 0..M {
        lsp[i] = (lsp_old[i].wrapping_sub(lsp_old[i] >> 2)).wrapping_add(lsp_new[i] >> 2);
    }
    lsp_az(&lsp, &mut az[0..]);
    half_sum(lsp_new, lsp_old, &mut lsp);
    lsp_az(&lsp, &mut az[MP1..]);
    for i in 0..M {
        lsp[i] = (lsp_new[i].wrapping_sub(lsp_new[i] >> 2)).wrapping_add(lsp_old[i] >> 2);
    }
    lsp_az(&lsp, &mut az[2 * MP1..]);
}

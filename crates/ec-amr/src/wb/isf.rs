//! Port of isp_isf.cpp, isp_az.cpp, interpolate_isp.cpp, weight_amrwb_lpc.cpp, qpisf_2s.cpp, qisf_ns.cpp.
use super::isf_tab::*;
use super::math::*;
use super::ops::*;

pub const ORDER: usize = 16;
pub const ISF_GAP: i16 = 128;
const MU: i32 = 10923;
const ALPHA: i16 = 29491;
const ONE_ALPHA: i16 = (32768 - 29491) as i16;
const MP1: usize = M + 1;

/// Ports `Isf_isp`.
pub fn isf_isp(isf: &[i16], isp: &mut [i16], m: i16) {
    let m = m as usize;
    for i in 0..m - 1 {
        isp[i] = isf[i];
    }
    isp[m - 1] = shl_int16(isf[m - 1], 1);
    for i in 0..m {
        let ind = (isp[i] >> 7) as usize;
        let offset = isp[i] & 0x007f;
        let l_tmp = mul_16by16_to_int32(
            ISP_COS_TABLE[ind + 1].wrapping_sub(ISP_COS_TABLE[ind]),
            offset,
        );
        isp[i] = add_int16(ISP_COS_TABLE[ind], (l_tmp >> 8) as i16);
    }
}

/// Ports `Get_isp_pol`. `isp` is already offset to the start of the ISP subset.
pub fn get_isp_pol(isp: &[i16], f: &mut [i32], n: i16) {
    f[0] = 0x0080_0000;
    f[1] = (-(isp[0] as i32)).wrapping_shl(9);
    let mut p = 2usize;
    let mut q = 2usize;
    for i in 2..=(n as usize) {
        f[p] = f[p - 2];
        for _ in 1..i {
            let mut t0 = fxp_mul32_by_16b(f[p - 1], isp[q] as i32);
            t0 = shl_int32(t0, 2);
            f[p] = f[p].wrapping_sub(t0);
            f[p] = f[p].wrapping_add(f[p - 2]);
            p -= 1;
        }
        f[p] = f[p].wrapping_sub((isp[q] as i32).wrapping_shl(9));
        p += i;
        q += 2;
    }
}

/// Ports `Get_isp_pol_16kHz`. `isp` is already offset to the start of the ISP subset.
pub fn get_isp_pol_16khz(isp: &[i16], f: &mut [i32], n: i16) {
    f[0] = 0x0020_0000;
    f[1] = (-(isp[0] as i32)).wrapping_shl(7);
    let mut p = 2usize;
    let mut q = 2usize;
    for i in 2..=(n as usize) {
        f[p] = f[p - 2];
        for _ in 1..i {
            let mut t0 = fxp_mul32_by_16b(f[p - 1], isp[q] as i32);
            t0 = shl_int32(t0, 2);
            f[p] = f[p].wrapping_sub(t0);
            f[p] = f[p].wrapping_add(f[p - 2]);
            p -= 1;
        }
        f[p] = f[p].wrapping_sub((isp[q] as i32).wrapping_shl(7));
        p += i;
        q += 2;
    }
}

/// Ports `Isp_Az`.
pub fn isp_az(isp: &[i16], a: &mut [i16], m: i16, adaptive_scaling: i16) {
    let mut f1 = [0i32; 11];
    let mut f2 = [0i32; 10];
    let nc = (m >> 1) as usize;
    let mu = m as usize;
    if nc > 8 {
        get_isp_pol_16khz(&isp[0..], &mut f1, nc as i16);
        for i in 0..=nc {
            f1[i] = shl_int32(f1[i], 2);
        }
        get_isp_pol_16khz(&isp[1..], &mut f2, nc as i16 - 1);
        for i in 0..=nc - 1 {
            f2[i] = shl_int32(f2[i], 2);
        }
    } else {
        get_isp_pol(&isp[0..], &mut f1, nc as i16);
        get_isp_pol(&isp[1..], &mut f2, nc as i16 - 1);
    }
    let mut i = nc as isize - 1;
    while i > 1 {
        let iu = i as usize;
        f2[iu] = f2[iu].wrapping_sub(f2[iu - 2]);
        i -= 1;
    }
    let ispm = isp[mu - 1] as i32;
    for i in 0..nc {
        let mut t0 = f1[i];
        let mut t1 = f2[i];
        t0 = fxp_mul32_by_16b(t0, ispm).wrapping_shl(1);
        t1 = fxp_mul32_by_16b(t1, ispm).wrapping_shl(1);
        f1[i] = f1[i].wrapping_add(t0);
        f2[i] = f2[i].wrapping_sub(t1);
    }
    a[0] = 4096;
    let mut tmax: i32 = 1;
    let mut j = mu - 1;
    for i in 1..nc {
        let mut t0 = add_int32(f1[i], f2[i]);
        let mut t1 = t0.wrapping_sub((t0 < 0) as i32);
        t1 ^= t1 >> 31;
        tmax |= t1;
        a[i] = ((t0 >> 12).wrapping_add((t0 >> 11) & 1)) as i16;
        t0 = sub_int32(f1[i], f2[i]);
        t1 = t0.wrapping_sub((t0 < 0) as i32);
        t1 ^= t1 >> 31;
        tmax |= t1;
        a[j] = ((t0 >> 12).wrapping_add((t0 >> 11) & 1)) as i16;
        j = j.wrapping_sub(1);
    }
    let mut q: i16 = if adaptive_scaling == 1 {
        4 - normalize_amr_wb(tmax)
    } else {
        0
    };
    let q_sug: i16;
    if q > 0 {
        q_sug = 12 + q;
        let mut j = mu - 1;
        for i in 1..nc {
            let mut t0 = add_int32(f1[i], f2[i]);
            a[i] = ((t0 >> q_sug).wrapping_add((t0 >> (q_sug - 1)) & 1)) as i16;
            t0 = sub_int32(f1[i], f2[i]);
            a[j] = ((t0 >> q_sug).wrapping_add((t0 >> (q_sug - 1)) & 1)) as i16;
            j = j.wrapping_sub(1);
        }
        a[0] = ((a[0] as i32) >> (q as u32).min(31)) as i16;
    } else {
        q_sug = 12;
        q = 0;
    }
    let mut t0 = ((((f1[nc] as i64) * (isp[mu - 1] as i64)) >> 16) as i32).wrapping_shl(1);
    t0 = add_int32(f1[nc], t0);
    a[nc] = ((t0 >> q_sug).wrapping_add((t0 >> (q_sug - 1)) & 1)) as i16;
    a[mu] = shr_rnd(isp[mu - 1], 3 + q);
}

/// Ports `interpolate_isp`. `az` must hold 4*(M+1) values.
pub fn interpolate_isp(isp_old: &[i16], isp_new: &[i16], frac: &[i16], az: &mut [i16]) {
    let mut isp = [0i16; M];
    for k in 0..3 {
        let fac_new = frac[k];
        let fac_old = add_int16(sub_int16(32767, fac_new), 1);
        for i in 0..M {
            let mut l_tmp = mul_16by16_to_int32(isp_old[i], fac_old);
            l_tmp = mac_16by16_to_int32(l_tmp, isp_new[i], fac_new);
            isp[i] = amr_wb_round(l_tmp);
        }
        isp_az(&isp, &mut az[k * MP1..], M as i16, 0);
    }
    isp_az(isp_new, &mut az[3 * MP1..], M as i16, 0);
}

/// Ports `weight_amrwb_lpc`.
pub fn weight_amrwb_lpc(a: &[i16], ap: &mut [i16], gamma: i16, m: i16) {
    let m = m as usize;
    let round: i32 = 0x4000;
    ap[0] = a[0];
    let mut fac = gamma;
    let mut i = 1;
    while i < m {
        ap[i] = (fxp_mac_16by16(a[i], fac, round) >> 15) as i16;
        fac = (fxp_mac_16by16(fac, gamma, round) >> 15) as i16;
        i += 1;
    }
    ap[i] = (fxp_mac_16by16(a[i], fac, round) >> 15) as i16;
}

/// Ports `Reorder_isf`.
pub fn reorder_isf(isf: &mut [i16], min_dist: i16, n: i16) {
    let mut isf_min = min_dist;
    for i in 0..(n as usize - 1) {
        if isf[i] < isf_min {
            isf[i] = isf_min;
        }
        isf_min = add_int16(isf[i], min_dist);
    }
}

fn bad_frame(isf_q: &mut [i16], past_isfq: &mut [i16], isfold: &[i16], isf_buf: &[i16]) {
    let mut ref_isf = [0i16; M];
    for i in 0..M {
        let mut l_tmp = mul_16by16_to_int32(mean_isf[i], 8192);
        for j in 0..L_MEANBUF {
            l_tmp = mac_16by16_to_int32(l_tmp, isf_buf[j * M + i], 8192);
        }
        ref_isf[i] = amr_wb_round(l_tmp);
    }
    for i in 0..ORDER {
        isf_q[i] = add_int16(
            mult_int16(ALPHA, isfold[i]),
            mult_int16(ONE_ALPHA, ref_isf[i]),
        );
    }
    for i in 0..ORDER {
        let tmp = add_int16(ref_isf[i], mult_int16(past_isfq[i], MU as i16));
        past_isfq[i] = sub_int16(isf_q[i], tmp);
        past_isfq[i] >>= 1;
    }
}

fn update_buf(isf_buf: &mut [i16], isf_q: &[i16]) {
    for i in 0..M {
        for j in (1..L_MEANBUF).rev() {
            isf_buf[j * M + i] = isf_buf[(j - 1) * M + i];
        }
        isf_buf[i] = isf_q[i];
    }
}

/// Ports `Dpisf_2s_46b`.
pub fn dpisf_2s_46b(
    indice: &[i16],
    isf_q: &mut [i16],
    past_isfq: &mut [i16],
    isfold: &[i16],
    isf_buf: &mut [i16],
    bfi: i16,
    enc_dec: i16,
) {
    let ix = |k: usize| indice[k] as i32;
    if bfi == 0 {
        for i in 0..9 {
            isf_q[i] = dico1_isf[((ix(0) << 3) + ix(0)) as usize + i];
        }
        for i in 0..7 {
            isf_q[i + 9] = dico2_isf[((ix(1) << 3) - ix(1)) as usize + i];
        }
        let mut i = 0;
        while i < 3 {
            isf_q[i] = isf_q[i].wrapping_add(dico21_isf[(ix(2) * 3) as usize + i]);
            isf_q[i + 3] = isf_q[i + 3].wrapping_add(dico22_isf[(ix(3) * 3) as usize + i]);
            isf_q[i + 6] = isf_q[i + 6].wrapping_add(dico23_isf[(ix(4) * 3) as usize + i]);
            isf_q[i + 9] = isf_q[i + 9].wrapping_add(dico24_isf[(ix(5) * 3) as usize + i]);
            isf_q[i + 12] = isf_q[i + 12].wrapping_add(dico25_isf[((ix(6) << 2) as usize) + i]);
            i += 1;
        }
        isf_q[i + 12] = isf_q[i + 12].wrapping_add(dico25_isf[((ix(6) << 2) as usize) + i]);
        for i in 0..ORDER {
            let tmp = isf_q[i];
            isf_q[i] = isf_q[i].wrapping_add(mean_isf[i]);
            isf_q[i] = isf_q[i].wrapping_add(((MU * past_isfq[i] as i32) >> 15) as i16);
            past_isfq[i] = tmp;
        }
        if enc_dec != 0 {
            update_buf(isf_buf, isf_q);
        }
    } else {
        bad_frame(isf_q, past_isfq, isfold, isf_buf);
    }
    reorder_isf(isf_q, ISF_GAP, ORDER as i16);
}

/// Ports `Dpisf_2s_36b`.
pub fn dpisf_2s_36b(
    indice: &[i16],
    isf_q: &mut [i16],
    past_isfq: &mut [i16],
    isfold: &[i16],
    isf_buf: &mut [i16],
    bfi: i16,
    enc_dec: i16,
) {
    let ix = |k: usize| indice[k] as i32;
    if bfi == 0 {
        for i in 0..9 {
            isf_q[i] = dico1_isf[(ix(0) * 9) as usize + i];
        }
        for i in 0..7 {
            isf_q[i + 9] = add_int16(
                dico2_isf[(ix(1) * 7) as usize + i],
                dico23_isf_36b[(ix(4) * 7) as usize + i],
            );
        }
        for i in 0..5 {
            isf_q[i] = add_int16(isf_q[i], dico21_isf_36b[(ix(2) * 5) as usize + i]);
        }
        for i in 0..4 {
            isf_q[i + 5] = add_int16(isf_q[i + 5], dico22_isf_36b[((ix(3) << 2) as usize) + i]);
        }
        for i in 0..ORDER {
            let tmp = isf_q[i];
            isf_q[i] = add_int16(tmp, mean_isf[i]);
            isf_q[i] = add_int16(isf_q[i], mult_int16(MU as i16, past_isfq[i]));
            past_isfq[i] = tmp;
        }
        if enc_dec != 0 {
            update_buf(isf_buf, isf_q);
        }
    } else {
        bad_frame(isf_q, past_isfq, isfold, isf_buf);
    }
    reorder_isf(isf_q, ISF_GAP, ORDER as i16);
}

/// Ports `Disf_ns`.
pub fn disf_ns(indice: &[i16], isf_q: &mut [i16]) {
    let ix = |k: usize| indice[k] as usize;
    isf_q[0] = dico1_isf_noise[ix(0) << 1];
    isf_q[1] = dico1_isf_noise[(ix(0) << 1) + 1];
    for i in 0..3 {
        isf_q[i + 2] = dico2_isf_noise[(ix(1) << 1) + ix(1) + i];
        isf_q[i + 5] = dico3_isf_noise[(ix(2) << 1) + ix(2) + i];
    }
    for i in 0..4 {
        isf_q[i + 8] = dico4_isf_noise[(ix(3) << 2) + i];
        isf_q[i + 12] = dico5_isf_noise[(ix(4) << 2) + i];
    }
    for i in 0..ORDER {
        isf_q[i] = add_int16(isf_q[i], mean_isf_noise[i]);
    }
    reorder_isf(isf_q, ISF_GAP, ORDER as i16);
}

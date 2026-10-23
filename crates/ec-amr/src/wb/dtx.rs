//! AMR-WB decoder DTX / comfort noise (port of dtx_decoder_amr_wb.cpp and dtx.h).
#![allow(dead_code)]

use super::bits::{Prms, Serial_parm, Serial_parm_1bit};
use super::ops::*;

pub const DTX_MAX_EMPTY_THRESH: i16 = 50;
pub const DTX_HIST_SIZE: usize = 8;
pub const DTX_HIST_SIZE_MIN_ONE: usize = 7;
pub const DTX_ELAPSED_FRAMES_THRESH: i16 = 24 + 7 - 1;
pub const DTX_HANG_CONST: i16 = 7;
pub const INV_MED_THRESH: i16 = 14564;
pub const ISF_GAP: i16 = 128;
pub const ISF_DITH_GAP: i16 = 448;
pub const ISF_FACTOR_LOW: i16 = 256;
pub const ISF_FACTOR_STEP: i16 = 2;
pub const GAIN_THR: i16 = 180;
pub const GAIN_FACTOR: i16 = 75;

pub const SPEECH: i16 = 0;
pub const DTX: i16 = 1;
pub const DTX_MUTE: i16 = 2;

pub const TX_SPEECH: i16 = 0;
pub const TX_SID_FIRST: i16 = 1;
pub const TX_SID_UPDATE: i16 = 2;
pub const TX_NO_DATA: i16 = 3;

pub const RX_SPEECH_GOOD: i16 = 0;
pub const RX_SPEECH_PROBABLY_DEGRADED: i16 = 1;
pub const RX_SPEECH_LOST: i16 = 2;
pub const RX_SPEECH_BAD: i16 = 3;
pub const RX_SID_FIRST: i16 = 4;
pub const RX_SID_UPDATE: i16 = 5;
pub const RX_SID_BAD: i16 = 6;
pub const RX_NO_DATA: i16 = 7;

#[derive(Clone)]
pub struct DtxEncState {
    pub isf_hist: [i16; M * DTX_HIST_SIZE],
    pub log_en_hist: [i16; DTX_HIST_SIZE],
    pub hist_ptr: i16,
    pub log_en_index: i16,
    pub cng_seed: i16,
    pub dtxHangoverCount: i16,
    pub decAnaElapsedCount: i16,
    pub D: [i32; 28],
    pub sumD: [i32; DTX_HIST_SIZE],
}

#[derive(Clone)]
pub struct DtxDecState {
    pub since_last_sid: i16,
    pub true_sid_period_inv: i16,
    pub log_en: i16,
    pub old_log_en: i16,
    pub level: i16,
    pub isf: [i16; M],
    pub isf_old: [i16; M],
    pub cng_seed: i16,
    pub isf_hist: [i16; M * DTX_HIST_SIZE],
    pub log_en_hist: [i16; DTX_HIST_SIZE],
    pub hist_ptr: i16,
    pub dtxHangoverCount: i16,
    pub decAnaElapsedCount: i16,
    pub sid_frame: i16,
    pub valid_data: i16,
    pub dtxHangoverAdded: i16,
    pub dtxGlobalState: i16,
    pub data_updated: i16,
    pub dither_seed: i16,
    pub CN_dith: i16,
}

impl Default for DtxDecState {
    fn default() -> Self {
        let mut s = DtxDecState {
            since_last_sid: 0,
            true_sid_period_inv: 0,
            log_en: 0,
            old_log_en: 0,
            level: 0,
            isf: [0; M],
            isf_old: [0; M],
            cng_seed: 0,
            isf_hist: [0; M * DTX_HIST_SIZE],
            log_en_hist: [0; DTX_HIST_SIZE],
            hist_ptr: 0,
            dtxHangoverCount: 0,
            decAnaElapsedCount: 0,
            sid_frame: 0,
            valid_data: 0,
            dtxHangoverAdded: 0,
            dtxGlobalState: 0,
            data_updated: 0,
            dither_seed: 0,
            CN_dith: 0,
        };
        s.reset(&[0; M]);
        s
    }
}

impl DtxDecState {
    pub fn reset(&mut self, isf_init: &[i16]) {
        dtx_dec_amr_wb_reset(self, isf_init);
    }
}

/// Port of `dtx_dec_amr_wb_reset`.
pub fn dtx_dec_amr_wb_reset(st: &mut DtxDecState, isf_init: &[i16]) -> i16 {
    st.since_last_sid = 0;
    st.true_sid_period_inv = 1 << 13;
    st.log_en = 3500;
    st.old_log_en = 3500;
    st.cng_seed = RANDOM_INITSEED;
    st.hist_ptr = 0;
    st.isf.copy_from_slice(&isf_init[..M]);
    st.isf_old.copy_from_slice(&isf_init[..M]);
    for i in 0..DTX_HIST_SIZE {
        st.isf_hist[i * M..i * M + M].copy_from_slice(&isf_init[..M]);
        st.log_en_hist[i] = st.log_en;
    }
    st.dtxHangoverCount = DTX_HANG_CONST;
    st.decAnaElapsedCount = 32767;
    st.sid_frame = 0;
    st.valid_data = 0;
    st.dtxHangoverAdded = 0;
    st.dtxGlobalState = SPEECH;
    st.data_updated = 0;
    st.dither_seed = RANDOM_INITSEED;
    st.CN_dith = 0;
    0
}

/// Port of `dtx_dec_amr_wb`.
pub fn dtx_dec_amr_wb(
    st: &mut DtxDecState,
    exc2: &mut [i16],
    new_state: i16,
    isf: &mut [i16],
    prms: &mut Prms,
) -> i16 {
    use super::math::*;
    let mut ind = [0i16; 7];
    let mut l_isf = [0i32; M];

    if st.dtxHangoverAdded != 0 && st.sid_frame != 0 {
        let mut ptr = st.hist_ptr as usize + 1;
        if ptr == DTX_HIST_SIZE {
            ptr = 0;
        }
        let hp = st.hist_ptr as usize;
        st.isf_hist.copy_within(hp * M..hp * M + M, ptr * M);
        st.log_en_hist[ptr] = st.log_en_hist[hp];

        st.log_en = 0;
        for i in 0..M {
            l_isf[i] = 0;
        }
        for i in 0..DTX_HIST_SIZE {
            st.log_en = add_int16(st.log_en, st.log_en_hist[i]);
            for j in 0..M {
                l_isf[j] = add_int32(l_isf[j], st.isf_hist[i * M + j] as i32);
            }
        }
        st.log_en >>= 1;
        st.log_en = st.log_en.wrapping_add(1024);
        if st.log_en < 0 {
            st.log_en = 0;
        }
        for j in 0..M {
            st.isf[j] = (l_isf[j] >> 3) as i16;
        }
    }

    if st.sid_frame != 0 {
        st.isf_old = st.isf;
        st.old_log_en = st.log_en;
        if st.valid_data != 0 {
            let mut tmp_int_length = st.since_last_sid;
            if tmp_int_length > 32 {
                tmp_int_length = 32;
            }
            if tmp_int_length >= 2 {
                st.true_sid_period_inv = div_16by16(1 << 10, shl_int16(tmp_int_length, 10));
            } else {
                st.true_sid_period_inv = 1 << 14;
            }
            ind[0] = Serial_parm(6, prms);
            ind[1] = Serial_parm(6, prms);
            ind[2] = Serial_parm(6, prms);
            ind[3] = Serial_parm(5, prms);
            ind[4] = Serial_parm(5, prms);
            super::isf::disf_ns(&mut ind, &mut st.isf);
            let log_en_index = Serial_parm(6, prms);
            st.CN_dith = Serial_parm_1bit(prms);
            st.log_en = shl_int16(log_en_index, 15 - 6);
            st.log_en = mult_int16(st.log_en, 12483);
            if st.data_updated == 0 || st.dtxGlobalState == SPEECH {
                st.isf_old = st.isf;
                st.old_log_en = st.log_en;
            }
        }
    }

    if st.sid_frame != 0 && st.valid_data != 0 {
        st.since_last_sid = 0;
    }

    let mut int_fac = shl_int16(st.since_last_sid, 10);
    int_fac = mult_int16(int_fac, st.true_sid_period_inv);
    if int_fac > 1024 {
        int_fac = 1024;
    }
    int_fac = shl_int16(int_fac, 4);
    let mut l_log_en_int = mul_16by16_to_int32(int_fac, st.log_en);
    for i in 0..M {
        isf[i] = mult_int16(int_fac, st.isf[i]);
    }
    int_fac = 16384i16.wrapping_sub(int_fac);
    l_log_en_int = mac_16by16_to_int32(l_log_en_int, int_fac, st.old_log_en);
    for i in 0..M {
        isf[i] = add_int16(isf[i], mult_int16(int_fac, st.isf_old[i]));
        isf[i] = shl_int16(isf[i], 1);
    }

    if st.CN_dith != 0 {
        CN_dithering(isf, &mut l_log_en_int, &mut st.dither_seed);
    }

    l_log_en_int >>= 9;
    let mut log_en_int_e = extract_h(l_log_en_int);
    let log_en_int_m = (sub_int32(l_log_en_int, l_deposit_h(log_en_int_e)) >> 1) as i16;
    log_en_int_e = log_en_int_e.wrapping_add(15);
    let mut level32 = power_of_2(log_en_int_e, log_en_int_m);
    let mut exp0 = normalize_amr_wb(level32);
    level32 = level32.wrapping_shl(exp0 as u32);
    exp0 = 15i16.wrapping_sub(exp0);
    let level = (level32 >> 16) as i16;

    for i in 0..L_FRAME {
        exc2[i] = super::exc::noise_gen_amrwb(&mut st.cng_seed) >> 4;
    }

    let mut exp: i16 = 0;
    let mut ener32 = dot_product12(exc2, exc2, L_FRAME as i16, &mut exp);
    one_ov_sqrt_norm(&mut ener32, &mut exp);
    let mut gain = extract_h(ener32);
    gain = mult_int16(level, gain);
    exp = exp.wrapping_add(exp0);
    exp = exp.wrapping_add(4);
    for i in 0..L_FRAME {
        let tmp = mult_int16(exc2[i], gain);
        exc2[i] = shl_int16(tmp, exp);
    }

    if new_state == DTX_MUTE {
        let mut tmp_int_length = st.since_last_sid;
        if tmp_int_length > 32 {
            tmp_int_length = 32;
        }
        st.true_sid_period_inv = div_16by16(1 << 10, shl_int16(tmp_int_length, 10));
        st.since_last_sid = 0;
        st.old_log_en = st.log_en;
        st.log_en = st.log_en.wrapping_sub(64);
        if st.log_en < 0 {
            st.log_en = 0;
        }
    }

    if st.sid_frame != 0 && (st.valid_data != 0 || (st.valid_data == 0 && st.dtxHangoverAdded != 0))
    {
        st.since_last_sid = 0;
        st.data_updated = 1;
    }
    0
}

/// Port of `dtx_dec_amr_wb_activity_update`.
pub fn dtx_dec_amr_wb_activity_update(st: &mut DtxDecState, isf: &[i16], exc: &[i16]) {
    use super::math::*;
    st.hist_ptr = st.hist_ptr.wrapping_add(1);
    if st.hist_ptr as usize == DTX_HIST_SIZE {
        st.hist_ptr = 0;
    }
    let hp = st.hist_ptr as usize;
    st.isf_hist[hp * M..hp * M + M].copy_from_slice(&isf[..M]);

    let mut l_frame_en: i32 = 0;
    for i in 0..L_FRAME {
        l_frame_en = mac_16by16_to_int32(l_frame_en, exc[i], exc[i]);
    }
    l_frame_en >>= 1;

    let mut log_en_e: i16 = 0;
    let mut log_en_m: i16 = 0;
    amrwb_log_2(l_frame_en, &mut log_en_e, &mut log_en_m);
    let mut log_en = shl_int16(log_en_e, 7);
    log_en = log_en.wrapping_add(log_en_m >> 8);
    log_en = log_en.wrapping_sub(1024);
    st.log_en_hist[hp] = log_en;
}

/// Port of `rx_amr_wb_dtx_handler`.
pub fn rx_amr_wb_dtx_handler(st: &mut DtxDecState, frame_type: i16) -> i16 {
    let mut new_state: i16;
    let enc_state: i16;

    if frame_type == RX_SID_FIRST
        || frame_type == RX_SID_UPDATE
        || frame_type == RX_SID_BAD
        || ((st.dtxGlobalState == DTX || st.dtxGlobalState == DTX_MUTE)
            && (frame_type == RX_NO_DATA
                || frame_type == RX_SPEECH_BAD
                || frame_type == RX_SPEECH_LOST))
    {
        new_state = DTX;
        if st.dtxGlobalState == DTX_MUTE
            && (frame_type == RX_SID_BAD
                || frame_type == RX_SID_FIRST
                || frame_type == RX_SPEECH_LOST
                || frame_type == RX_NO_DATA)
        {
            new_state = DTX_MUTE;
        }
        st.since_last_sid = add_int16(st.since_last_sid, 1);
        if st.since_last_sid > DTX_MAX_EMPTY_THRESH {
            new_state = DTX_MUTE;
        }
    } else {
        new_state = SPEECH;
        st.since_last_sid = 0;
    }

    if st.data_updated == 0 && frame_type == RX_SID_UPDATE {
        st.decAnaElapsedCount = 0;
    }
    st.decAnaElapsedCount = add_int16(st.decAnaElapsedCount, 1);
    st.dtxHangoverAdded = 0;

    if frame_type == RX_SID_FIRST
        || frame_type == RX_SID_UPDATE
        || frame_type == RX_SID_BAD
        || frame_type == RX_NO_DATA
    {
        enc_state = DTX;
    } else {
        enc_state = SPEECH;
    }

    if enc_state == SPEECH {
        st.dtxHangoverCount = DTX_HANG_CONST;
    } else if st.decAnaElapsedCount > DTX_ELAPSED_FRAMES_THRESH {
        st.dtxHangoverAdded = 1;
        st.decAnaElapsedCount = 0;
        st.dtxHangoverCount = 0;
    } else if st.dtxHangoverCount == 0 {
        st.decAnaElapsedCount = 0;
    } else {
        st.dtxHangoverCount -= 1;
    }

    if new_state != SPEECH {
        st.sid_frame = 0;
        st.valid_data = 0;
        if frame_type == RX_SID_FIRST {
            st.sid_frame = 1;
        } else if frame_type == RX_SID_UPDATE {
            st.sid_frame = 1;
            st.valid_data = 1;
        } else if frame_type == RX_SID_BAD {
            st.sid_frame = 1;
            st.dtxHangoverAdded = 0;
        }
    }
    new_state
}

/// Port of `aver_isf_history`.
pub fn aver_isf_history(isf_old: &mut [i16], indices: &[i16], isf_aver: &mut [i32]) {
    let mut isf_tmp = [0i16; 2 * M];
    for k in 0..2 {
        if indices[k] as i32 + 1 != 0 {
            let ik = indices[k] as usize * M;
            let i2 = indices[2] as usize * M;
            for i in 0..M {
                isf_tmp[k * M + i] = isf_old[ik + i];
                isf_old[ik + i] = isf_old[i2 + i];
            }
        }
    }
    for j in 0..M {
        let mut l_tmp: i32 = 0;
        for i in 0..DTX_HIST_SIZE {
            l_tmp = add_int32(l_tmp, isf_old[i * M + j] as i32);
        }
        isf_aver[j] = l_tmp;
    }
    for k in 0..2 {
        if indices[k] as i32 + 1 != 0 {
            let ik = indices[k] as usize * M;
            for i in 0..M {
                isf_old[ik + i] = isf_tmp[k * M + i];
            }
        }
    }
}

/// Port of `find_frame_indices`.
pub fn find_frame_indices(isf_old_tx: &[i16], indices: &mut [i16], st: &mut DtxEncState) {
    let mut tmp: i32 = DTX_HIST_SIZE_MIN_ONE as i32;
    let mut j: i32 = -1;
    for i in 0..DTX_HIST_SIZE_MIN_ONE {
        j += tmp;
        st.sumD[i] = sub_int32(st.sumD[i], st.D[j as usize]);
        tmp -= 1;
    }
    for i in (1..=DTX_HIST_SIZE_MIN_ONE).rev() {
        st.sumD[i] = st.sumD[i - 1];
    }
    st.sumD[0] = 0;

    tmp = 0;
    let mut i: i32 = 27;
    while i >= 12 {
        tmp += 1;
        let mut jj = tmp;
        while jj > 0 {
            st.D[(i - jj + 1) as usize] = st.D[(i - jj - tmp) as usize];
            jj -= 1;
        }
        i -= tmp;
    }

    let mut ptr = st.hist_ptr as i32;
    let hp = st.hist_ptr as usize;
    for i in 1..DTX_HIST_SIZE {
        ptr -= 1;
        if ptr < 0 {
            ptr = DTX_HIST_SIZE_MIN_ONE as i32;
        }
        let mut l_tmp: i32 = 0;
        for j in 0..M {
            let t = sub_int16(isf_old_tx[hp * M + j], isf_old_tx[ptr as usize * M + j]);
            l_tmp = mac_16by16_to_int32(l_tmp, t, t);
        }
        st.D[i - 1] = l_tmp;
        st.sumD[0] = add_int32(st.sumD[0], st.D[i - 1]);
        st.sumD[i] = add_int32(st.sumD[i], st.D[i - 1]);
    }

    let mut summax = st.sumD[0];
    let mut summin = st.sumD[0];
    indices[0] = 0;
    indices[2] = 0;
    for i in 1..DTX_HIST_SIZE {
        if st.sumD[i] > summax {
            indices[0] = i as i16;
            summax = st.sumD[i];
        }
        if st.sumD[i] < summin {
            indices[2] = i as i16;
            summin = st.sumD[i];
        }
    }

    let mut summax2nd: i32 = -2147483647;
    indices[1] = -1;
    for i in 0..DTX_HIST_SIZE {
        if st.sumD[i] > summax2nd && i as i16 != indices[0] {
            indices[1] = i as i16;
            summax2nd = st.sumD[i];
        }
    }

    for i in 0..3 {
        indices[i] = sub_int16(st.hist_ptr, indices[i]);
        if indices[i] < 0 {
            indices[i] = add_int16(indices[i], DTX_HIST_SIZE as i16);
        }
    }

    let t = normalize_amr_wb(summax);
    summax = summax.wrapping_shl(t as u32);
    summin = summin.wrapping_shl(t as u32);
    let mut l_tmp = mul_16by16_to_int32(amr_wb_round(summax), INV_MED_THRESH);
    if l_tmp <= summin {
        indices[0] = -1;
    }
    summax2nd = shl_int32(summax2nd, t);
    l_tmp = mul_16by16_to_int32(amr_wb_round(summax2nd), INV_MED_THRESH);
    if l_tmp <= summin {
        indices[1] = -1;
    }
}

/// Port of `dithering_control`.
pub fn dithering_control(st: &DtxEncState) -> i16 {
    let mut isf_diff: i32 = 0;
    for i in 0..8 {
        isf_diff = add_int32(isf_diff, st.sumD[i]);
    }
    let mut cn_dith: i16 = if (isf_diff >> 26) > 0 { 1 } else { 0 };

    let mut mean: i16 = 0;
    for i in 0..DTX_HIST_SIZE {
        mean = add_int16(mean, st.log_en_hist[i]);
    }
    mean >>= 3;
    let mut gain_diff: i16 = 0;
    for i in 0..DTX_HIST_SIZE {
        let mut tmp = sub_int16(st.log_en_hist[i], mean);
        tmp = tmp.wrapping_sub((tmp < 0) as i16);
        gain_diff = gain_diff.wrapping_add(tmp ^ (tmp >> 15));
    }
    if gain_diff > GAIN_THR {
        cn_dith = 1;
    }
    cn_dith
}

/// Port of `CN_dithering`.
pub fn CN_dithering(isf: &mut [i16], l_log_en_int: &mut i32, dither_seed: &mut i16) {
    use super::exc::noise_gen_amrwb;
    use super::math::mult_int16_r;

    let mut rand_dith = noise_gen_amrwb(dither_seed) >> 1;
    let mut rand_dith2 = noise_gen_amrwb(dither_seed) >> 1;
    rand_dith = rand_dith.wrapping_add(rand_dith2);
    *l_log_en_int = add_int32(*l_log_en_int, mul_16by16_to_int32(rand_dith, GAIN_FACTOR));
    if *l_log_en_int < 0 {
        *l_log_en_int = 0;
    }

    let mut dither_fac = ISF_FACTOR_LOW;
    rand_dith = noise_gen_amrwb(dither_seed) >> 1;
    rand_dith2 = noise_gen_amrwb(dither_seed) >> 1;
    rand_dith = rand_dith.wrapping_add(rand_dith2);
    let mut temp = add_int16(isf[0], mult_int16_r(rand_dith, dither_fac));
    if temp < ISF_GAP {
        isf[0] = ISF_GAP;
    } else {
        isf[0] = temp;
    }
    for i in 1..M - 1 {
        dither_fac = add_int16(dither_fac, ISF_FACTOR_STEP);
        rand_dith = noise_gen_amrwb(dither_seed) >> 1;
        rand_dith2 = noise_gen_amrwb(dither_seed) >> 1;
        rand_dith = rand_dith.wrapping_add(rand_dith2);
        temp = add_int16(isf[i], mult_int16_r(rand_dith, dither_fac));
        let temp1 = sub_int16(temp, isf[i - 1]);
        if temp1 < ISF_DITH_GAP {
            isf[i] = isf[i - 1].wrapping_add(ISF_DITH_GAP);
        } else {
            isf[i] = temp;
        }
    }
    if isf[M - 2] > 16384 {
        isf[M - 2] = 16384;
    }
}

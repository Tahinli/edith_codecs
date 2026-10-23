//! Decoder-side DTX / error-concealment helpers: dtx_dec, lsp_avg, bgnscd, c_g_aver,
//! ec_gains, ex_ctrl, pseudonoise / build_CN_code.
#![allow(clippy::too_many_arguments, clippy::needless_range_loop)]

use super::basic::*;
use super::bitstream::RxFrameType;
use super::excitation::{gc_pred_average_limited, gc_pred_update, GcPredState};
use super::lpc::{d_plsf_3, init_d_plsf_3, lsf_lsp, lsp_az, lsp_lsf, reorder_lsf, DPlsfState};
use super::post::{a_refl, syn_filt};
use super::tables::{Mode, LSF_GAP, L_FRAME, L_SUBFR, M, MEAN_LSF_5};

pub const DTX_MAX_EMPTY_THRESH: i16 = 50;
pub const DTX_HIST_SIZE: usize = 8;
pub const DTX_HANG_CONST: i16 = 7;
pub const DTX_ELAPSED_FRAMES_THRESH: i16 = 24 + 7 - 1;
pub const L_ENERGYHIST: usize = 60;
pub const L_CBGAINHIST: usize = 7;
const FRAMEENERGYLIMIT: i16 = 17578;
const LOWERNOISELIMIT: i16 = 20;
const UPPERNOISELIMIT: i16 = 1953;
const EXPCONST: i16 = 5243;
const PN_INITIAL_SEED: i32 = 0x70816958;

/// enum DTXStateType.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DtxStateType {
    Speech = 0,
    Dtx = 1,
    DtxMute = 2,
}

// ---------------------------------------------------------------- pseudonoise

/// pseudonoise(): LFSR noise bits; `shift_reg` is the 32-bit seed (L_pn_seed_rx).
pub fn pseudonoise(shift_reg: &mut i32, no_bits: i16) -> i16 {
    let mut noise_bits: i16 = 0;
    for _ in 0..no_bits {
        let mut sn: i16 = if *shift_reg & 1 != 0 { 1 } else { 0 };
        if *shift_reg & 0x1000_0000 != 0 {
            sn ^= 1;
        }
        noise_bits <<= 1;
        noise_bits |= (*shift_reg & 1) as i16;
        *shift_reg >>= 1;
        if sn & 1 != 0 {
            *shift_reg |= 0x4000_0000;
        }
    }
    noise_bits
}

/// build_CN_code(): fills cod[0..L_SUBFR] with comfort-noise pulses.
pub fn build_cn_code(seed: &mut i32, cod: &mut [i16]) {
    for c in cod[..L_SUBFR].iter_mut() {
        *c = 0;
    }
    for k in 0..10i16 {
        let i = pseudonoise(seed, 2);
        let temp = l_mult(i, 10) as i16;
        let i = add(temp >> 1, k);
        let j = pseudonoise(seed, 1);
        cod[i as usize] = if j > 0 { 4096 } else { -4096 };
    }
}

// ------------------------------------------------------------------- lsp_avg

/// lsp_avgState.
#[derive(Clone, Debug)]
pub struct LspAvgState {
    pub lsp_mean_save: [i16; M],
}

impl LspAvgState {
    /// lsp_avg_reset (initialised from mean_lsf_5).
    pub fn new() -> Self {
        let mut s = [0i16; M];
        s.copy_from_slice(&MEAN_LSF_5[..M]);
        Self { lsp_mean_save: s }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for LspAvgState {
    fn default() -> Self {
        Self::new()
    }
}

/// lsp_avg(): updates the running mean LSP in `st` using `lsp[0..M]`.
pub fn lsp_avg(st: &mut LspAvgState, lsp: &[i16]) {
    for i in 0..M {
        let mut l_tmp = (st.lsp_mean_save[i] as i32) << 16;
        l_tmp = l_msu(l_tmp, EXPCONST, st.lsp_mean_save[i]);
        l_tmp = l_mac(l_tmp, EXPCONST, lsp[i]);
        st.lsp_mean_save[i] = round_(l_tmp);
    }
}

// -------------------------------------------------------------------- bgnscd

/// Bgn_scdState.
#[derive(Clone, Debug)]
pub struct BgnScdState {
    pub frame_energy_hist: [i16; L_ENERGYHIST],
    pub bg_hangover: i16,
}

impl BgnScdState {
    pub fn new() -> Self {
        Self { frame_energy_hist: [0; L_ENERGYHIST], bg_hangover: 0 }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for BgnScdState {
    fn default() -> Self {
        Self::new()
    }
}

/// Bgn_scd(): `ltp_gain_hist` has 9 entries, `speech` has L_FRAME samples.
/// Returns inbgNoise (0/1); updates `voiced_hangover`.
pub fn bgn_scd(st: &mut BgnScdState, ltp_gain_hist: &[i16], speech: &[i16], voiced_hangover: &mut i16) -> i16 {
    let mut s: i32 = 0;
    for i in (0..L_FRAME).rev() {
        let mut l_temp = (speech[i] as i32) * (speech[i] as i32);
        if l_temp != 0x4000_0000 {
            l_temp <<= 1;
        } else {
            l_temp = i32::MAX;
        }
        s = l_add(s, l_temp);
    }
    let curr_energy: i16 = if s > 0x1fff_ffff { i16::MAX } else { (s >> 14) as i16 };

    let mut frame_energy_min: i16 = 32767;
    for i in (0..L_ENERGYHIST).rev() {
        if st.frame_energy_hist[i] < frame_energy_min {
            frame_energy_min = st.frame_energy_hist[i];
        }
    }
    let l_temp = (frame_energy_min as i32) << 4;
    let noise_floor: i16 = if l_temp != (l_temp as i16) as i32 {
        if l_temp > 0 { i16::MAX } else { i16::MIN }
    } else {
        l_temp as i16
    };

    let mut max_energy = st.frame_energy_hist[0];
    for i in (1..=(L_ENERGYHIST - 5)).rev() {
        if max_energy < st.frame_energy_hist[i] {
            max_energy = st.frame_energy_hist[i];
        }
    }
    let mut max_energy_last_part = st.frame_energy_hist[2 * L_ENERGYHIST / 3];
    for i in (2 * L_ENERGYHIST / 3 + 1)..L_ENERGYHIST {
        if max_energy_last_part < st.frame_energy_hist[i] {
            max_energy_last_part = st.frame_energy_hist[i];
        }
    }

    if max_energy > LOWERNOISELIMIT
        && curr_energy < FRAMEENERGYLIMIT
        && curr_energy > LOWERNOISELIMIT
        && (curr_energy < noise_floor || max_energy_last_part < UPPERNOISELIMIT)
    {
        if st.bg_hangover + 1 > 30 {
            st.bg_hangover = 30;
        } else {
            st.bg_hangover += 1;
        }
    } else {
        st.bg_hangover = 0;
    }
    let in_bg_noise: i16 = if st.bg_hangover > 1 { 1 } else { 0 };

    for i in 0..(L_ENERGYHIST - 1) {
        st.frame_energy_hist[i] = st.frame_energy_hist[i + 1];
    }
    st.frame_energy_hist[L_ENERGYHIST - 1] = curr_energy;

    let ltp_limit: i16 = if st.bg_hangover > 15 {
        16383
    } else if st.bg_hangover > 8 {
        15565
    } else {
        13926
    };
    let mut prev_voiced = gmed_n(&ltp_gain_hist[4..], 5) > ltp_limit;
    if st.bg_hangover > 20 {
        prev_voiced = gmed_n(ltp_gain_hist, 9) > ltp_limit;
    }
    if prev_voiced {
        *voiced_hangover = 0;
    } else {
        let temp = *voiced_hangover + 1;
        *voiced_hangover = if temp > 10 { 10 } else { temp };
    }
    in_bg_noise
}

// ------------------------------------------------------------------- c_g_aver

/// Cb_gain_averageState.
#[derive(Clone, Debug)]
pub struct CbGainAverageState {
    pub cb_gain_history: [i16; L_CBGAINHIST],
    pub hang_var: i16,
    pub hang_count: i16,
}

impl CbGainAverageState {
    pub fn new() -> Self {
        Self { cb_gain_history: [0; L_CBGAINHIST], hang_var: 0, hang_count: 0 }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for CbGainAverageState {
    fn default() -> Self {
        Self::new()
    }
}

/// Cb_gain_average(): lsp/lsp_aver have M entries; bfi/prev_bf/pdfi/prev_pdf/
/// in_background_noise are 0/1 flags. Returns the mixed code-book gain.
pub fn cb_gain_average(
    st: &mut CbGainAverageState,
    mode: Mode,
    gain_code: i16,
    lsp: &[i16],
    lsp_aver: &[i16],
    bfi: i16,
    prev_bf: i16,
    pdfi: i16,
    prev_pdf: i16,
    in_background_noise: i16,
    voiced_hangover: i16,
) -> i16 {
    let mut cb_gain_mix = gain_code;
    for i in 0..(L_CBGAINHIST - 1) {
        st.cb_gain_history[i] = st.cb_gain_history[i + 1];
    }
    st.cb_gain_history[L_CBGAINHIST - 1] = gain_code;

    let mut diff: i16 = 0;
    let mut tmp = [0i16; M];
    for i in 0..M {
        let tmp1 = abs_s(sub(lsp_aver[i], lsp[i]));
        let shift1 = norm_s(tmp1) - 1;
        let tmp1 = shl(tmp1, shift1);
        let shift2 = norm_s(lsp_aver[i]);
        let tmp2 = shl(lsp_aver[i], shift2);
        tmp[i] = div_s(tmp1, tmp2);
        let shift = 2 + shift1 - shift2;
        if shift >= 0 {
            tmp[i] = shr(tmp[i], shift);
        } else {
            tmp[i] = shl(tmp[i], negate(shift));
        }
        diff = add(diff, tmp[i]);
    }

    if diff > 5325 {
        st.hang_var = st.hang_var.wrapping_add(1);
    } else {
        st.hang_var = 0;
    }
    if st.hang_var > 10 {
        st.hang_count = 0;
    }

    let m = mode as usize;
    let (mr475, mr515, mr59, mr67, mr102) =
        (Mode::Mr475 as usize, Mode::Mr515 as usize, Mode::Mr59 as usize, Mode::Mr67 as usize, Mode::Mr102 as usize);
    let low_mode = m == mr475 || m == mr515 || m == mr59;

    if m <= mr67 || m == mr102 {
        let tmp_diff: i16 = if (((pdfi != 0 && prev_pdf != 0) || bfi != 0 || prev_bf != 0)
            && voiced_hangover > 1
            && in_background_noise != 0)
            && low_mode
        {
            diff.wrapping_sub(4506)
        } else {
            diff.wrapping_sub(3277)
        };
        let tmp1: i16 = if tmp_diff > 0 { tmp_diff } else { 0 };
        let mut bg_mix: i16 = if 2048 < tmp1 { 8192 } else { shl(tmp1, 2) };
        if st.hang_count < 40 || diff > 5325 {
            bg_mix = 8192;
        }

        let mut l_sum = l_mult(6554, st.cb_gain_history[2]);
        for i in 3..L_CBGAINHIST {
            l_sum = l_mac(l_sum, 6554, st.cb_gain_history[i]);
        }
        let mut cb_gain_mean = round_(l_sum);

        if (bfi != 0 || prev_bf != 0) && in_background_noise != 0 && low_mode {
            l_sum = l_mult(4681, st.cb_gain_history[0]);
            for i in 1..L_CBGAINHIST {
                l_sum = l_mac(l_sum, 4681, st.cb_gain_history[i]);
            }
            cb_gain_mean = round_(l_sum);
        }

        l_sum = l_mult(bg_mix, cb_gain_mix);
        l_sum = l_mac(l_sum, 8192, cb_gain_mean);
        l_sum = l_msu(l_sum, bg_mix, cb_gain_mean);
        cb_gain_mix = round_(l_shl(l_sum, 2));
    }
    st.hang_count = st.hang_count.wrapping_add(1);
    cb_gain_mix
}

// ------------------------------------------------------------------- ex_ctrl

/// Ex_ctrl(): `excitation` has L_SUBFR samples (modified in place);
/// `ex_energy_hist` has 9 entries. Always returns 0.
pub fn ex_ctrl(
    excitation: &mut [i16],
    mut exc_energy: i16,
    ex_energy_hist: &[i16],
    voiced_hangover: i16,
    prev_bfi: i16,
    careful_flag: i16,
) -> i16 {
    let mut avg_energy = gmed_n(ex_energy_hist, 9);
    let mut prev_energy = ((ex_energy_hist[7] as i32 + ex_energy_hist[8] as i32) >> 1) as i16;
    if ex_energy_hist[8] < prev_energy {
        prev_energy = ex_energy_hist[8];
    }
    if exc_energy < avg_energy && exc_energy > 5 {
        let mut test_energy = shl(prev_energy, 2);
        if voiced_hangover < 7 || prev_bfi != 0 {
            test_energy = sub(test_energy, prev_energy);
        }
        if avg_energy > test_energy {
            avg_energy = test_energy;
        }
        let exp = norm_s(exc_energy);
        exc_energy = shl(exc_energy, exp);
        exc_energy = div_s(16383, exc_energy);
        let mut t0 = l_mult(avg_energy, exc_energy);
        t0 = l_shr(t0, sub(20, exp));
        if t0 > 32767 {
            t0 = 32767;
        }
        let mut scale_factor = t0 as i16;
        if careful_flag != 0 && scale_factor > 3072 {
            scale_factor = 3072;
        }
        for i in 0..L_SUBFR {
            let t = l_shr(l_mult(scale_factor, excitation[i]), 11);
            excitation[i] = t as i16;
        }
    }
    0
}

// ------------------------------------------------------------------ ec_gains

/// ec_gain_codeState.
#[derive(Clone, Debug)]
pub struct EcGainCodeState {
    pub gbuf: [i16; 5],
    pub past_gain_code: i16,
    pub prev_gc: i16,
}

impl EcGainCodeState {
    pub fn new() -> Self {
        Self { gbuf: [1; 5], past_gain_code: 0, prev_gc: 1 }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for EcGainCodeState {
    fn default() -> Self {
        Self::new()
    }
}

/// ec_gain_code(): `state` is the concealment state index 0..6; returns gain_code
/// and updates the gain predictor state.
pub fn ec_gain_code(st: &EcGainCodeState, pred_state: &mut GcPredState, state: i16) -> i16 {
    const CDOWN: [i16; 7] = [32767, 32112, 32112, 32112, 32112, 32112, 22937];
    let mut tmp = gmed_n(&st.gbuf, 5);
    if sub(tmp, st.past_gain_code) > 0 {
        tmp = st.past_gain_code;
    }
    let gain_code = mult(tmp, CDOWN[state as usize]);
    let (qua_ener_mr122, qua_ener) = gc_pred_average_limited(pred_state);
    gc_pred_update(pred_state, qua_ener_mr122, qua_ener);
    gain_code
}

/// ec_gain_code_update(): `gain_code` may be clamped in place.
pub fn ec_gain_code_update(st: &mut EcGainCodeState, bfi: i16, prev_bf: i16, gain_code: &mut i16) {
    if bfi == 0 {
        if prev_bf != 0 && sub(*gain_code, st.prev_gc) > 0 {
            *gain_code = st.prev_gc;
        }
        st.prev_gc = *gain_code;
    }
    st.past_gain_code = *gain_code;
    for i in 1..5 {
        st.gbuf[i - 1] = st.gbuf[i];
    }
    st.gbuf[4] = *gain_code;
}

/// ec_gain_pitchState.
#[derive(Clone, Debug)]
pub struct EcGainPitchState {
    pub pbuf: [i16; 5],
    pub past_gain_pit: i16,
    pub prev_gp: i16,
}

impl EcGainPitchState {
    pub fn new() -> Self {
        Self { pbuf: [1640; 5], past_gain_pit: 0, prev_gp: 16384 }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for EcGainPitchState {
    fn default() -> Self {
        Self::new()
    }
}

/// ec_gain_pitch(): `state` 0..6; returns the concealed pitch gain.
pub fn ec_gain_pitch(st: &EcGainPitchState, state: i16) -> i16 {
    const PDOWN: [i16; 7] = [32767, 32112, 32112, 26214, 9830, 6553, 6553];
    let mut tmp = gmed_n(&st.pbuf, 5);
    if sub(tmp, st.past_gain_pit) > 0 {
        tmp = st.past_gain_pit;
    }
    mult(tmp, PDOWN[state as usize])
}

/// ec_gain_pitch_update(): `gain_pitch` may be clamped in place.
pub fn ec_gain_pitch_update(st: &mut EcGainPitchState, bfi: i16, prev_bf: i16, gain_pitch: &mut i16) {
    if bfi == 0 {
        if prev_bf != 0 && sub(*gain_pitch, st.prev_gp) > 0 {
            *gain_pitch = st.prev_gp;
        }
        st.prev_gp = *gain_pitch;
    }
    st.past_gain_pit = *gain_pitch;
    if sub(st.past_gain_pit, 16384) > 0 {
        st.past_gain_pit = 16384;
    }
    for i in 1..5 {
        st.pbuf[i - 1] = st.pbuf[i];
    }
    st.pbuf[4] = st.past_gain_pit;
}

// ------------------------------------------------------------------- dtx_dec

const LSF_HIST_MEAN_SCALE: [i16; M] = [20000, 20000, 20000, 20000, 20000, 18000, 16384, 8192, 0, 0];
const DTX_LOG_EN_ADJUST: [i16; 9] = [-1023, -878, -732, -586, -440, -294, -148, 0, 0];

const LSP_INIT: [i16; M] = [30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000];
const LSF_HIST_INIT: [i16; M] = [1384, 2077, 3420, 5108, 6742, 8122, 9863, 11092, 12714, 13701];

/// dtx_decState.
#[derive(Clone, Debug)]
pub struct DtxDecState {
    pub since_last_sid: i16,
    pub true_sid_period_inv: i16,
    pub log_en: i16,
    pub old_log_en: i16,
    pub l_pn_seed_rx: i32,
    pub lsp: [i16; M],
    pub lsp_old: [i16; M],
    pub lsf_hist: [i16; M * DTX_HIST_SIZE],
    pub lsf_hist_ptr: i16,
    pub lsf_hist_mean: [i16; M * DTX_HIST_SIZE],
    pub log_pg_mean: i16,
    pub log_en_hist: [i16; DTX_HIST_SIZE],
    pub log_en_hist_ptr: i16,
    pub log_en_adjust: i16,
    pub dtx_hangover_count: i16,
    pub dec_ana_elapsed_count: i16,
    pub sid_frame: i16,
    pub valid_data: i16,
    pub dtx_hangover_added: i16,
    /// contains previous state
    pub dtx_global_state: DtxStateType,
    /// marker to know if CNI data is ever renewed
    pub data_updated: i16,
}

impl DtxDecState {
    /// dtx_dec_reset.
    pub fn new() -> Self {
        let mut lsf_hist = [0i16; M * DTX_HIST_SIZE];
        for i in 0..DTX_HIST_SIZE {
            lsf_hist[M * i..M * (i + 1)].copy_from_slice(&LSF_HIST_INIT);
        }
        Self {
            since_last_sid: 0,
            true_sid_period_inv: 1 << 13,
            log_en: 3500,
            old_log_en: 3500,
            l_pn_seed_rx: PN_INITIAL_SEED,
            lsp: LSP_INIT,
            lsp_old: LSP_INIT,
            lsf_hist,
            lsf_hist_ptr: 0,
            lsf_hist_mean: [0; M * DTX_HIST_SIZE],
            log_pg_mean: 0,
            log_en_hist: [3500; DTX_HIST_SIZE],
            log_en_hist_ptr: 0,
            log_en_adjust: 0,
            dtx_hangover_count: DTX_HANG_CONST,
            dec_ana_elapsed_count: 32767,
            sid_frame: 0,
            valid_data: 0,
            dtx_hangover_added: 0,
            dtx_global_state: DtxStateType::Dtx,
            data_updated: 0,
        }
    }
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for DtxDecState {
    fn default() -> Self {
        Self::new()
    }
}

fn sat16_shl10(v: i16) -> i16 {
    let l = (v as i32) << 10;
    if l != (l as i16) as i32 {
        if v > 0 { i16::MAX } else { i16::MIN }
    } else {
        l as i16
    }
}

/// dtx_dec(): generates comfort noise. `mem_syn` is the M-entry synthesis filter
/// memory, `parm` the decoded SID parameter array (parm[0] = LSF predictor init,
/// parm[1..4] = LSF indices, parm[4] = log energy index; as in C `parm`),
/// `synth` receives L_FRAME samples, `a_t` receives 4*(M+1) LPC coefficients.
pub fn dtx_dec(
    st: &mut DtxDecState,
    mem_syn: &mut [i16],
    lsf_state: &mut DPlsfState,
    pred_state: &mut GcPredState,
    aver_state: &mut CbGainAverageState,
    new_state: DtxStateType,
    mode: Mode,
    parm: &[i16],
    synth: &mut [i16],
    a_t: &mut [i16],
) {
    let mode_i = mode as usize;

    if st.dtx_hangover_added != 0 && st.sid_frame != 0 {
        // Update log_en_adjust; average LSF history and energy history.
        st.log_en_adjust = DTX_LOG_EN_ADJUST[mode_i];

        let mut ptr = st.lsf_hist_ptr + M as i16;
        if ptr == 80 {
            ptr = 0;
        }
        let src = st.lsf_hist_ptr as usize;
        st.lsf_hist.copy_within(src..src + M, ptr as usize);

        let mut ptr = st.log_en_hist_ptr + 1;
        if ptr == DTX_HIST_SIZE as i16 {
            ptr = 0;
        }
        st.log_en_hist[ptr as usize] = st.log_en_hist[st.log_en_hist_ptr as usize];

        st.log_en = 0;
        let mut l_lsf = [0i32; M];
        for i in (0..DTX_HIST_SIZE).rev() {
            let h = st.log_en_hist[i];
            let temp: i16 = if h < 0 { !((!h) >> 3) } else { h >> 3 };
            st.log_en = add(st.log_en, temp);
            for j in (0..M).rev() {
                l_lsf[j] = l_add(l_lsf[j], st.lsf_hist[i * M + j] as i32);
            }
        }
        let mut lsf = [0i16; M];
        for j in (0..M).rev() {
            lsf[j] = if l_lsf[j] < 0 { (!((!l_lsf[j]) >> 3)) as i16 } else { (l_lsf[j] >> 3) as i16 };
        }
        lsf_lsp(&lsf, &mut st.lsp, M);
        st.log_en = sub(st.log_en, st.log_en_adjust);

        // Calculate lsf_hist_mean.
        st.lsf_hist_mean.copy_from_slice(&st.lsf_hist);
        for i in (0..M).rev() {
            let mut l_lsf_mean: i32 = 0;
            for j in (0..8).rev() {
                l_lsf_mean = l_add(l_lsf_mean, st.lsf_hist_mean[i + j * M] as i32);
            }
            let lsf_mean: i16 =
                if l_lsf_mean < 0 { (!((!l_lsf_mean) >> 3)) as i16 } else { (l_lsf_mean >> 3) as i16 };
            for j in (0..8).rev() {
                let idx = i + j * M;
                st.lsf_hist_mean[idx] = sub(st.lsf_hist_mean[idx], lsf_mean);
                st.lsf_hist_mean[idx] = mult(st.lsf_hist_mean[idx], LSF_HIST_MEAN_SCALE[i]);
                let negative = st.lsf_hist_mean[idx] < 0;
                st.lsf_hist_mean[idx] = abs_s(st.lsf_hist_mean[idx]);
                if st.lsf_hist_mean[idx] > 655 {
                    st.lsf_hist_mean[idx] = 655 + ((st.lsf_hist_mean[idx] - 655) >> 2);
                }
                if st.lsf_hist_mean[idx] > 1310 {
                    st.lsf_hist_mean[idx] = 1310;
                }
                if negative {
                    st.lsf_hist_mean[idx] = -st.lsf_hist_mean[idx];
                }
            }
        }
    }

    if st.sid_frame != 0 {
        st.lsp_old = st.lsp;
        st.old_log_en = st.log_en;
        if st.valid_data != 0 {
            let mut tmp_int_length = st.since_last_sid;
            st.since_last_sid = 0;
            if tmp_int_length >= 32 {
                tmp_int_length = 32;
            }
            let temp = sat16_shl10(tmp_int_length);
            if tmp_int_length >= 2 {
                st.true_sid_period_inv = div_s(1 << 10, temp);
            } else {
                st.true_sid_period_inv = 1 << 14;
            }

            init_d_plsf_3(lsf_state, parm[0]);
            d_plsf_3(lsf_state, Mode::MrDtx, 0, &parm[1..], &mut st.lsp);
            lsf_state.past_r_q = [0; M];

            let log_en_index = parm[4];
            if log_en_index > 63 || log_en_index < -64 {
                st.log_en = if log_en_index > 0 { i16::MAX } else { i16::MIN };
            } else {
                st.log_en = log_en_index << (11 - 2);
            }
            st.log_en = st.log_en.wrapping_sub(2560 * 2);
            if log_en_index == 0 {
                st.log_en = i16::MIN;
            }

            if st.data_updated == 0 || st.dtx_global_state == DtxStateType::Speech {
                st.lsp_old = st.lsp;
                st.old_log_en = st.log_en;
            }
        }

        let temp: i16 = if st.log_en < 0 { !((!st.log_en) >> 1) } else { st.log_en >> 1 };
        let mut ma_pred_init = temp.wrapping_sub(9000);
        if ma_pred_init > 0 {
            ma_pred_init = 0;
        } else if ma_pred_init < -14436 {
            ma_pred_init = -14436;
        }
        pred_state.past_qua_en = [ma_pred_init; 4];
        let ma = ((ma_pred_init as i32 * 5443) >> 15) as i16;
        pred_state.past_qua_en_mr122 = [ma; 4];
    }

    // Log energy adjustment.
    let adj = DTX_LOG_EN_ADJUST[mode_i];
    let mut temp: i16 = if adj > 1023 {
        i16::MAX
    } else if adj < -1024 {
        i16::MIN
    } else {
        ((((adj as i32) << 5) * 3277) >> 15) as i16
    };
    temp = if temp < 0 { !((!temp) >> 5) } else { temp >> 5 };
    st.log_en_adjust = add((((st.log_en_adjust as i32) * 29491) >> 15) as i16, temp);

    // Interpolation factor.
    let mut int_fac = shl(st.since_last_sid + 1, 10);
    int_fac = mult(int_fac, st.true_sid_period_inv);
    if int_fac > 1024 {
        int_fac = 16384;
    } else if int_fac < -2048 {
        int_fac = i16::MIN;
    } else {
        int_fac <<= 4;
    }

    let mut l_log_en_int = l_mult(int_fac, st.log_en);
    let mut lsp_int = [0i16; M];
    for i in (0..M).rev() {
        lsp_int[i] = mult(int_fac, st.lsp[i]);
    }
    int_fac = 16384 - int_fac;
    l_log_en_int = l_mac(l_log_en_int, int_fac, st.old_log_en);
    for i in (0..M).rev() {
        lsp_int[i] = add(lsp_int[i], mult(int_fac, st.lsp_old[i]));
        let l_temp = (lsp_int[i] as i32) << 1;
        lsp_int[i] = if l_temp != (l_temp as i16) as i32 {
            if lsp_int[i] > 0 { i16::MAX } else { i16::MIN }
        } else {
            l_temp as i16
        };
    }

    // LSF variability.
    let mut lsf_variab_factor = st.log_pg_mean.wrapping_sub(2457);
    lsf_variab_factor = 4096 - mult(lsf_variab_factor, 9830);
    if lsf_variab_factor > 4095 {
        lsf_variab_factor = i16::MAX;
    } else if lsf_variab_factor < 0 {
        lsf_variab_factor = 0;
    } else {
        lsf_variab_factor <<= 3;
    }
    let lsf_variab_index = pseudonoise(&mut st.l_pn_seed_rx, 3) as usize;

    let mut lsf_int = [0i16; M];
    lsp_lsf(&lsp_int, &mut lsf_int, M);
    let mut lsf_int_variab = lsf_int;
    for i in (0..M).rev() {
        lsf_int_variab[i] =
            add(lsf_int_variab[i], mult(lsf_variab_factor, st.lsf_hist_mean[i + lsf_variab_index * M]));
    }
    reorder_lsf(&mut lsf_int, LSF_GAP, M);
    reorder_lsf(&mut lsf_int_variab, LSF_GAP, M);

    lsf_state.past_lsf_q = lsf_int;

    let mut lsp_int2 = [0i16; M];
    let mut lsp_int_variab = [0i16; M];
    lsf_lsp(&lsf_int, &mut lsp_int2, M);
    lsf_lsp(&lsf_int_variab, &mut lsp_int_variab, M);
    let lsp_int = lsp_int2;

    let mut acoeff = [0i16; M + 1];
    let mut acoeff_variab = [0i16; M + 1];
    lsp_az(&lsp_int, &mut acoeff);
    lsp_az(&lsp_int_variab, &mut acoeff_variab);

    for k in 0..4 {
        a_t[k * (M + 1)..(k + 1) * (M + 1)].copy_from_slice(&acoeff);
    }

    // Prediction gain.
    let mut refl = [0i16; M];
    a_refl(&acoeff[1..], &mut refl);
    let mut pred_err: i16 = i16::MAX;
    for i in 0..M {
        let l_temp = ((refl[i] as i32) * (refl[i] as i32)) >> 15;
        let temp: i16 = if l_temp <= 0x7fff { i16::MAX - l_temp as i16 } else { 0 };
        pred_err = mult(pred_err, temp);
    }
    let mut log_pg_e = 0i16;
    let mut log_pg_m = 0i16;
    log2(pred_err as i32, &mut log_pg_e, &mut log_pg_m);
    let mut log_pg = shl(log_pg_e - 15, 12);
    log_pg = shr(sub(0, add(log_pg, shr(log_pg_m, 15 - 12))), 1);
    st.log_pg_mean = add(mult(29491, st.log_pg_mean), mult(3277, log_pg));

    l_log_en_int = l_shr(l_log_en_int, 10);
    l_log_en_int = l_add(l_log_en_int, 4 * 65536);
    l_log_en_int = l_sub(l_log_en_int, l_shl(log_pg as i32, 4));
    l_log_en_int = l_add(l_log_en_int, l_shl(st.log_en_adjust as i32, 5));

    let log_en_int_e = (l_log_en_int >> 16) as i16;
    let log_en_int_m = l_shr(l_sub(l_log_en_int, (log_en_int_e as i32) << 16), 1) as i16;
    let level = pow2(log_en_int_e, log_en_int_m) as i16;

    let mut ex = [0i16; L_SUBFR];
    for i in 0..4 {
        build_cn_code(&mut st.l_pn_seed_rx, &mut ex);
        for j in (0..L_SUBFR).rev() {
            ex[j] = mult(level, ex[j]);
        }
        syn_filt(&acoeff_variab, &ex, &mut synth[i * L_SUBFR..(i + 1) * L_SUBFR], L_SUBFR, mem_syn, true);
    }

    aver_state.hang_var = 20;
    aver_state.hang_count = 0;

    if new_state == DtxStateType::DtxMute {
        let mut tmp_int_length = st.since_last_sid;
        if tmp_int_length > 32 {
            tmp_int_length = 32;
        } else if tmp_int_length <= 0 {
            tmp_int_length = 8;
        }
        let temp = sat16_shl10(tmp_int_length);
        st.true_sid_period_inv = div_s(1 << 10, temp);
        st.since_last_sid = 0;
        st.lsp_old = st.lsp;
        st.old_log_en = st.log_en;
        st.log_en = st.log_en.wrapping_sub(256);
        if st.log_en < 0 {
            st.log_en = 0;
        }
    }

    if st.sid_frame != 0 && (st.valid_data != 0 || (st.valid_data == 0 && st.dtx_hangover_added != 0)) {
        st.since_last_sid = 0;
        st.data_updated = 1;
    }
}

/// dtx_dec_activity_update(): `lsf` has M entries, `frame` has L_FRAME samples.
pub fn dtx_dec_activity_update(st: &mut DtxDecState, lsf: &[i16], frame: &[i16]) {
    st.lsf_hist_ptr += M as i16;
    if st.lsf_hist_ptr == 80 {
        st.lsf_hist_ptr = 0;
    }
    let p = st.lsf_hist_ptr as usize;
    st.lsf_hist[p..p + M].copy_from_slice(&lsf[..M]);

    let mut l_frame_en: i32 = 0;
    for i in (0..L_FRAME).rev() {
        let mut l_temp = (frame[i] as i32) * (frame[i] as i32);
        if l_temp != 0x4000_0000 {
            l_temp <<= 1;
        } else {
            l_temp = i32::MAX;
        }
        l_frame_en = l_add(l_frame_en, l_temp);
    }
    let mut log_en_e = 0i16;
    let mut log_en_m = 0i16;
    log2(l_frame_en, &mut log_en_e, &mut log_en_m);

    let l_temp = (log_en_e as i32) << 10;
    log_en_e = if l_temp != (l_temp as i16) as i32 {
        if log_en_e > 0 { i16::MAX } else { i16::MIN }
    } else {
        l_temp as i16
    };
    log_en_m = if log_en_m < 0 { !((!log_en_m) >> 5) } else { log_en_m >> 5 };
    let mut log_en = log_en_e.wrapping_add(log_en_m);
    log_en = log_en.wrapping_sub(7497 + 1024);

    st.log_en_hist_ptr += 1;
    if st.log_en_hist_ptr == DTX_HIST_SIZE as i16 {
        st.log_en_hist_ptr = 0;
    }
    st.log_en_hist[st.log_en_hist_ptr as usize] = log_en;
}

/// rx_dtx_handler(): returns the new DTX state.
pub fn rx_dtx_handler(st: &mut DtxDecState, frame_type: RxFrameType) -> DtxStateType {
    use RxFrameType::*;
    let mut new_state;
    if matches!(frame_type, SidFirst | SidUpdate | SidBad)
        || ((st.dtx_global_state == DtxStateType::Dtx || st.dtx_global_state == DtxStateType::DtxMute)
            && matches!(frame_type, NoData | SpeechBad | Onset))
    {
        new_state = DtxStateType::Dtx;
        if st.dtx_global_state == DtxStateType::DtxMute && matches!(frame_type, SidBad | SidFirst | Onset | NoData)
        {
            new_state = DtxStateType::DtxMute;
        }
        st.since_last_sid = st.since_last_sid.wrapping_add(1);
        if frame_type != SidUpdate && st.since_last_sid > DTX_MAX_EMPTY_THRESH {
            new_state = DtxStateType::DtxMute;
        }
    } else {
        new_state = DtxStateType::Speech;
        st.since_last_sid = 0;
    }

    if st.data_updated == 0 && frame_type == SidUpdate {
        st.dec_ana_elapsed_count = 0;
    }
    st.dec_ana_elapsed_count = add(st.dec_ana_elapsed_count, 1);
    st.dtx_hangover_added = 0;

    let mut enc_state;
    if matches!(frame_type, SidFirst | SidUpdate | SidBad | Onset | NoData) {
        enc_state = DtxStateType::Dtx;
        if frame_type == NoData && new_state == DtxStateType::Speech {
            enc_state = DtxStateType::Speech;
        }
    } else {
        enc_state = DtxStateType::Speech;
    }

    if enc_state == DtxStateType::Speech {
        st.dtx_hangover_count = DTX_HANG_CONST;
    } else if st.dec_ana_elapsed_count > DTX_ELAPSED_FRAMES_THRESH {
        st.dtx_hangover_added = 1;
        st.dec_ana_elapsed_count = 0;
        st.dtx_hangover_count = 0;
    } else if st.dtx_hangover_count == 0 {
        st.dec_ana_elapsed_count = 0;
    } else {
        st.dtx_hangover_count -= 1;
    }

    if new_state != DtxStateType::Speech {
        st.sid_frame = 0;
        st.valid_data = 0;
        if frame_type == SidFirst {
            st.sid_frame = 1;
        } else if frame_type == SidUpdate {
            st.sid_frame = 1;
            st.valid_data = 1;
        } else if frame_type == SidBad {
            st.sid_frame = 1;
            st.dtx_hangover_added = 0;
        }
    }
    new_state
}

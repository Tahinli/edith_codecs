//! AMR-NB decoder: port of opencore-amr `dec_amr.cpp` (Decoder_amr) and `sp_dec.cpp`
//! (GSMFrameDecode), plus the `AMRDecode` frame-type handling of the libopencore-amrnb wrapper.
#![allow(clippy::all)]

use super::basic::*;
use super::bitstream::{parse_frame, BitstreamError, RxFrameType};
use super::dtx::*;
use super::excitation::*;
use super::lpc::*;
use super::post::*;
use super::tables::*;
use ec_core::error::{Error, Result};
use ec_core::frame::{AudioFrame, ChannelLayout, Frame, SampleFormat};
use ec_core::packet::{Buf, Packet};
use ec_core::registry::{AudioParameters, CodecId, CodecParameters, Decoder, MediaParameters};

const EXC_ENERGY_HIST_LEN: usize = 9;
const LTP_GAIN_HISTORY_LEN: usize = 9;
const EXC_OFF: usize = PIT_MAX as usize + L_INTERPOL;

/// window_200_40 (build_CN_param noise source).
static WINDOW_200_40: [i16; 240] = [2621,2623,2629,2638,2651,2668,2689,2713,2741,2772,2808,2847,2890,2936,2986,3040,3097,3158,3223,3291,3363,3438,3517,3599,3685,3774,3867,3963,4063,4166,4272,4382,4495,4611,4731,4853,4979,5108,5240,5376,5514,5655,5800,5947,6097,6250,6406,6565,6726,6890,7057,7227,7399,7573,7750,7930,8112,8296,8483,8672,8863,9057,9252,9450,9650,9852,10055,10261,10468,10677,10888,11101,11315,11531,11748,11967,12187,12409,12632,12856,13082,13308,13536,13764,13994,14225,14456,14688,14921,15155,15389,15624,15859,16095,16331,16568,16805,17042,17279,17516,17754,17991,18228,18465,18702,18939,19175,19411,19647,19882,20117,20350,20584,20816,21048,21279,21509,21738,21967,22194,22420,22644,22868,23090,23311,23531,23749,23965,24181,24394,24606,24816,25024,25231,25435,25638,25839,26037,26234,26428,26621,26811,26999,27184,27368,27548,27727,27903,28076,28247,28415,28581,28743,28903,29061,29215,29367,29515,29661,29804,29944,30081,30214,30345,30472,30597,30718,30836,30950,31062,31170,31274,31376,31474,31568,31659,31747,31831,31911,31988,32062,32132,32198,32261,32320,32376,32428,32476,32521,32561,32599,32632,32662,32688,32711,32729,32744,32755,32763,32767,32767,32741,32665,32537,32359,32129,31850,31521,31143,30716,30242,29720,29151,28538,27879,27177,26433,25647,24821,23957,23055,22117,21145,20139,19102,18036,16941,15820,14674,13505,12315,11106,9879,8637,7381,6114,4838,3554,2264,971];

fn energy_new(inp: &[i16]) -> i32 {
    let mut s: i32 = 0;
    for &x in inp {
        s = l_mac(s, x, x);
    }
    if s != i32::MAX {
        s >>= 4;
    } else {
        // C energy_old
        s = 0;
        for &x in inp {
            let t = x >> 2;
            s = l_mac(s, t, t);
        }
    }
    s
}

/// C agc2: scale `sig_out` to the energy of `sig_in` (both 40 samples).
fn agc2(sig_in: &[i16], sig_out: &mut [i16]) {
    let l = sig_out.len();
    let mut s = energy_new(&sig_out[..l]);
    if s == 0 {
        return;
    }
    let mut exp = norm_l(s) - 1;
    let gain_out = round_(l_shl(s, exp));
    s = energy_new(&sig_in[..l]);
    let g0: i16;
    if s == 0 {
        g0 = 0;
    } else {
        let i = norm_l(s);
        let gain_in = round_(l_shl(s, i));
        exp -= i;
        let temp = div_s(gain_out, gain_in);
        let mut s = temp as i32;
        s = if s > 0x00FF_FFFF {
            i32::MAX
        } else if s < -16777216 {
            i32::MIN
        } else {
            s << 7
        };
        s = l_shr(s, exp);
        s = inv_sqrt(s);
        let lt = if s > 0x003F_FFFF {
            i32::MAX
        } else if s < -4194304 {
            i32::MIN
        } else {
            s << 9
        };
        g0 = round_(lt);
    }
    for i in (0..l).rev() {
        let lt = l_mult(sig_out[i], g0);
        sig_out[i] = if lt > 0x0FFF_FFFF {
            i16::MAX
        } else if lt < -268435456 {
            i16::MIN
        } else {
            (lt >> 13) as i16
        };
    }
}

/// C build_CN_param.
fn build_cn_param(seed: &mut i16, n_param: usize, sizes: &[i16], parm: &mut [i16]) {
    let mut lt = l_mult(*seed, 31821);
    lt >>= 1;
    *seed = l_add(lt, 13849) as i16;
    let base = (*seed & 0x7f) as usize;
    for i in 0..n_param {
        let mask = !(0xFFFFu32 << sizes[i]) as i16;
        parm[i] = WINDOW_200_40[base + i] & mask;
    }
}

fn bitno_for(mode: Mode) -> &'static [i16] {
    match mode {
        Mode::Mr475 => &BITNO_MR475,
        Mode::Mr515 => &BITNO_MR515,
        Mode::Mr59 => &BITNO_MR59,
        Mode::Mr67 => &BITNO_MR67,
        Mode::Mr74 => &BITNO_MR74,
        Mode::Mr795 => &BITNO_MR795,
        Mode::Mr102 => &BITNO_MR102,
        Mode::Mr122 => &BITNO_MR122,
        Mode::MrDtx => &BITNO_MRDTX,
    }
}

/// C `Decoder_amrState`.
#[derive(Clone, Debug)]
pub struct DecoderAmrState {
    pub old_exc: [i16; L_SUBFR + EXC_OFF],
    pub lsp_old: [i16; M],
    pub mem_syn: [i16; M],
    pub sharp: i16,
    pub old_t0: i16,
    pub prev_bf: i16,
    pub prev_pdf: i16,
    pub state: i16,
    pub exc_energy_hist: [i16; EXC_ENERGY_HIST_LEN],
    pub t0_lag_buff: i16,
    pub in_background_noise: i16,
    pub voiced_hangover: i16,
    pub ltp_gain_history: [i16; LTP_GAIN_HISTORY_LEN],
    pub background_state: BgnScdState,
    pub nodata_seed: i16,
    pub cb_gain_aver_state: CbGainAverageState,
    pub lsp_avg_st: LspAvgState,
    pub lsf_state: DPlsfState,
    pub ec_gain_p_st: EcGainPitchState,
    pub ec_gain_c_st: EcGainCodeState,
    pub pred_state: GcPredState,
    pub ph_disp_st: PhDispState,
    pub dtx_decoder_state: DtxDecState,
}

const LSP_OLD_INIT: [i16; M] = [30000, 26000, 21000, 15000, 8000, 0, -8000, -15000, -21000, -26000];

impl DecoderAmrState {
    pub fn new() -> Self {
        let mut s = DecoderAmrState {
            old_exc: [0; L_SUBFR + EXC_OFF],
            lsp_old: LSP_OLD_INIT,
            mem_syn: [0; M],
            sharp: SHARPMIN,
            old_t0: 40,
            prev_bf: 0,
            prev_pdf: 0,
            state: 0,
            exc_energy_hist: [0; EXC_ENERGY_HIST_LEN],
            t0_lag_buff: 40,
            in_background_noise: 0,
            voiced_hangover: 0,
            ltp_gain_history: [0; LTP_GAIN_HISTORY_LEN],
            background_state: BgnScdState::new(),
            nodata_seed: 21845,
            cb_gain_aver_state: CbGainAverageState::new(),
            lsp_avg_st: LspAvgState::new(),
            lsf_state: DPlsfState::new(),
            ec_gain_p_st: EcGainPitchState::new(),
            ec_gain_c_st: EcGainCodeState::new(),
            pred_state: GcPredState::new(),
            ph_disp_st: PhDispState::new(),
            dtx_decoder_state: DtxDecState::new(),
        };
        s.reset(Mode::Mr475);
        s
    }

    /// C Decoder_amr_reset (`mode == MrDtx` keeps mem_syn, lsp_old, excEnergyHist, lsp_avg,
    /// gc_pred and the DTX decoder state).
    pub fn reset(&mut self, mode: Mode) {
        self.old_exc[..EXC_OFF].fill(0);
        if mode != Mode::MrDtx {
            self.mem_syn = [0; M];
        }
        self.sharp = SHARPMIN;
        self.old_t0 = 40;
        if mode != Mode::MrDtx {
            self.lsp_old = LSP_OLD_INIT;
        }
        self.prev_bf = 0;
        self.prev_pdf = 0;
        self.state = 0;
        self.t0_lag_buff = 40;
        self.in_background_noise = 0;
        self.voiced_hangover = 0;
        if mode != Mode::MrDtx {
            self.exc_energy_hist = [0; EXC_ENERGY_HIST_LEN];
        }
        self.ltp_gain_history = [0; LTP_GAIN_HISTORY_LEN];
        self.cb_gain_aver_state.reset();
        if mode != Mode::MrDtx {
            self.lsp_avg_st.reset();
        }
        self.lsf_state.reset();
        self.ec_gain_p_st.reset();
        self.ec_gain_c_st.reset();
        if mode != Mode::MrDtx {
            self.pred_state.reset();
        }
        self.background_state.reset();
        self.nodata_seed = 21845;
        self.ph_disp_st.reset();
        if mode != Mode::MrDtx {
            self.dtx_decoder_state.reset();
        }
    }
}

impl Default for DecoderAmrState {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn sat_dbl(v: i16) -> i16 {
    let l = (v as i32) << 1;
    if l != (l as i16) as i32 {
        if v > 0 { i16::MAX } else { i16::MIN }
    } else {
        l as i16
    }
}

/// C Decoder_amr. `parm` has MAX_PRM_SIZE+1 entries, `synth` 160, `a_t` AZ_SIZE (44).
pub fn decoder_amr(
    st: &mut DecoderAmrState,
    mode: Mode,
    parm_in: &[i16],
    frame_type: RxFrameType,
    synth: &mut [i16],
    a_t: &mut [i16],
) {
    let mut parm = [0i16; MAX_PRM_SIZE + 1];
    parm.copy_from_slice(&parm_in[..MAX_PRM_SIZE + 1]);
    let mut lsp_new = [0i16; M];
    let mut lsp_mid = [0i16; M];
    let mut prev_lsf = [0i16; M];
    let mut lsf_i = [0i16; M];
    let mut code = [0i16; L_SUBFR];
    let mut excp = [0i16; L_SUBFR];
    let mut exc_enhanced = [0i16; L_SUBFR];
    let mut bfi: i16 = 0;
    let mut pdfi: i16 = 0;

    let new_dtx_state = rx_dtx_handler(&mut st.dtx_decoder_state, frame_type);

    if new_dtx_state != DtxStateType::Speech {
        st.reset(Mode::MrDtx);
        dtx_dec(
            &mut st.dtx_decoder_state,
            &mut st.mem_syn,
            &mut st.lsf_state,
            &mut st.pred_state,
            &mut st.cb_gain_aver_state,
            new_dtx_state,
            mode,
            &parm,
            synth,
            a_t,
        );
        let past = st.lsf_state.past_lsf_q;
        lsf_lsp(&past, &mut st.lsp_old, M);
        lsp_avg(&mut st.lsp_avg_st, &past);
        st.dtx_decoder_state.dtx_global_state = new_dtx_state;
        return;
    }

    if matches!(frame_type, RxFrameType::SpeechBad | RxFrameType::NoData | RxFrameType::Onset) {
        bfi = 1;
        if matches!(frame_type, RxFrameType::NoData | RxFrameType::Onset) {
            let mi = mode.index();
            build_cn_param(&mut st.nodata_seed, PRMNO[mi] as usize, bitno_for(mode), &mut parm);
        }
    } else if frame_type == RxFrameType::SpeechDegraded {
        pdfi = 1;
    }

    if bfi != 0 {
        st.state += 1;
    } else if st.state == 6 {
        st.state = 5;
    } else {
        st.state = 0;
    }
    if st.state > 6 {
        st.state = 6;
    }

    if st.dtx_decoder_state.dtx_global_state == DtxStateType::Dtx {
        st.state = 5;
        st.prev_bf = 0;
    } else if st.dtx_decoder_state.dtx_global_state == DtxStateType::DtxMute {
        st.state = 5;
        st.prev_bf = 1;
    }

    prev_lsf.copy_from_slice(&st.lsf_state.past_lsf_q);

    let mut p = 0usize;
    if mode != Mode::Mr122 {
        d_plsf_3(&mut st.lsf_state, mode, bfi, &parm[p..], &mut lsp_new);
        p += 3;
        int_lpc_1to3(&st.lsp_old, &lsp_new, a_t);
    } else {
        d_plsf_5(&mut st.lsf_state, bfi, &parm[p..], &mut lsp_mid, &mut lsp_new);
        p += 5;
        int_lpc_1and3(&st.lsp_old, &lsp_mid, &lsp_new, a_t);
    }
    st.lsp_old = lsp_new;

    let mut az_off = 0usize;
    let mut even_subfr: i16 = 0;
    let mut subfr_nr: i16 = -1;
    let mut t0: i16 = 0;
    let mut t0_frac: i16 = 0;
    let mut index_mr475: i16 = 0;
    let mut gain_pit: i16 = 0;
    let mut gain_code: i16 = 0;
    let mut pit_sharp: i16;

    let mut i_subfr = 0usize;
    while i_subfr < L_FRAME {
        subfr_nr += 1;
        even_subfr = 1 - even_subfr;

        let mut pit_flag = i_subfr as i16;
        if i_subfr == L_FRAME_BY2 && mode != Mode::Mr475 && mode != Mode::Mr515 {
            pit_flag = 0;
        }

        let mut index = parm[p];
        p += 1;

        if mode != Mode::Mr122 {
            let flag4: i16 = matches!(mode, Mode::Mr475 | Mode::Mr515 | Mode::Mr59 | Mode::Mr67) as i16;
            let (delta_frc_low, delta_frc_range): (i16, i16) =
                if mode == Mode::Mr795 { (10, 19) } else { (5, 9) };
            let mut t0_min = st.old_t0 - delta_frc_low;
            if t0_min < PIT_MIN {
                t0_min = PIT_MIN;
            }
            let mut t0_max = t0_min + delta_frc_range;
            if t0_max > PIT_MAX {
                t0_max = PIT_MAX;
                t0_min = t0_max - delta_frc_range;
            }
            dec_lag3(index, t0_min, t0_max, pit_flag, st.old_t0, &mut t0, &mut t0_frac, flag4);
            st.t0_lag_buff = t0;
            if bfi != 0 {
                if st.old_t0 < PIT_MAX {
                    st.old_t0 += 1;
                }
                t0 = st.old_t0;
                t0_frac = 0;
                if st.in_background_noise != 0
                    && st.voiced_hangover > 4
                    && matches!(mode, Mode::Mr475 | Mode::Mr515 | Mode::Mr59)
                {
                    t0 = st.t0_lag_buff;
                }
            }
            pred_lt_3or6(&mut st.old_exc, EXC_OFF, t0, t0_frac, L_SUBFR as i16, 1);
        } else {
            dec_lag6(index, PIT_MIN_MR122, PIT_MAX, pit_flag, &mut t0, &mut t0_frac);
            if !(bfi == 0 && (pit_flag == 0 || index < 61)) {
                st.t0_lag_buff = t0;
                t0 = st.old_t0;
                t0_frac = 0;
            }
            pred_lt_3or6(&mut st.old_exc, EXC_OFF, t0, t0_frac, L_SUBFR as i16, 0);
        }

        match mode {
            Mode::Mr475 | Mode::Mr515 => {
                index = parm[p];
                let sg = parm[p + 1];
                p += 2;
                decode_2i40_9bits(subfr_nr, sg, index, &mut code);
                pit_sharp = sat_dbl(st.sharp);
            }
            Mode::Mr59 => {
                index = parm[p];
                let sg = parm[p + 1];
                p += 2;
                decode_2i40_11bits(sg, index, &mut code);
                pit_sharp = sat_dbl(st.sharp);
            }
            Mode::Mr67 => {
                index = parm[p];
                let sg = parm[p + 1];
                p += 2;
                decode_3i40_14bits(sg, index, &mut code);
                pit_sharp = sat_dbl(st.sharp);
            }
            Mode::Mr74 | Mode::Mr795 => {
                index = parm[p];
                let sg = parm[p + 1];
                p += 2;
                decode_4i40_17bits(sg, index, &mut code);
                pit_sharp = sat_dbl(st.sharp);
            }
            Mode::Mr102 => {
                dec_8i40_31bits(&parm[p..], &mut code);
                p += 7;
                pit_sharp = sat_dbl(st.sharp);
            }
            _ => {
                index = parm[p];
                p += 1;
                if bfi != 0 {
                    gain_pit = ec_gain_pitch(&st.ec_gain_p_st, st.state);
                } else {
                    gain_pit = d_gain_pitch(mode, index);
                }
                ec_gain_pitch_update(&mut st.ec_gain_p_st, bfi, st.prev_bf, &mut gain_pit);
                dec_10i40_35bits(&parm[p..], &mut code);
                p += 10;
                pit_sharp = sat_dbl(gain_pit);
            }
        }

        // add the pitch contribution to code[]
        let mut i = t0 as usize;
        while i < L_SUBFR {
            let temp = mult(code[i - t0 as usize], pit_sharp);
            code[i] = add(code[i], temp);
            i += 1;
        }

        if mode == Mode::Mr475 {
            if even_subfr != 0 {
                index_mr475 = parm[p];
                p += 1;
            }
            if bfi == 0 {
                dec_gain(&mut st.pred_state, mode, index_mr475, &code, even_subfr, &mut gain_pit, &mut gain_code);
            } else {
                gain_pit = ec_gain_pitch(&st.ec_gain_p_st, st.state);
                gain_code = ec_gain_code(&st.ec_gain_c_st, &mut st.pred_state, st.state);
            }
            ec_gain_pitch_update(&mut st.ec_gain_p_st, bfi, st.prev_bf, &mut gain_pit);
            ec_gain_code_update(&mut st.ec_gain_c_st, bfi, st.prev_bf, &mut gain_code);
            pit_sharp = gain_pit;
            if pit_sharp > SHARPMAX {
                pit_sharp = SHARPMAX;
            }
        } else if (mode as usize) <= (Mode::Mr74 as usize) || mode == Mode::Mr102 {
            index = parm[p];
            p += 1;
            if bfi == 0 {
                dec_gain(&mut st.pred_state, mode, index, &code, even_subfr, &mut gain_pit, &mut gain_code);
            } else {
                gain_pit = ec_gain_pitch(&st.ec_gain_p_st, st.state);
                gain_code = ec_gain_code(&st.ec_gain_c_st, &mut st.pred_state, st.state);
            }
            ec_gain_pitch_update(&mut st.ec_gain_p_st, bfi, st.prev_bf, &mut gain_pit);
            ec_gain_code_update(&mut st.ec_gain_c_st, bfi, st.prev_bf, &mut gain_code);
            pit_sharp = gain_pit;
            if pit_sharp > SHARPMAX {
                pit_sharp = SHARPMAX;
            }
            if mode == Mode::Mr102 && st.old_t0 > (L_SUBFR as i16 + 5) {
                pit_sharp = if pit_sharp < 0 { !((!pit_sharp) >> 2) } else { pit_sharp >> 2 };
            }
        } else {
            index = parm[p];
            p += 1;
            if mode == Mode::Mr795 {
                if bfi != 0 {
                    gain_pit = ec_gain_pitch(&st.ec_gain_p_st, st.state);
                } else {
                    gain_pit = d_gain_pitch(mode, index);
                }
                ec_gain_pitch_update(&mut st.ec_gain_p_st, bfi, st.prev_bf, &mut gain_pit);
                index = parm[p];
                p += 1;
                if bfi == 0 {
                    gain_code = d_gain_code(&mut st.pred_state, mode, index, &code);
                } else {
                    gain_code = ec_gain_code(&st.ec_gain_c_st, &mut st.pred_state, st.state);
                }
                ec_gain_code_update(&mut st.ec_gain_c_st, bfi, st.prev_bf, &mut gain_code);
                pit_sharp = gain_pit;
                if pit_sharp > SHARPMAX {
                    pit_sharp = SHARPMAX;
                }
            } else {
                // MR122
                if bfi == 0 {
                    gain_code = d_gain_code(&mut st.pred_state, mode, index, &code);
                } else {
                    gain_code = ec_gain_code(&st.ec_gain_c_st, &mut st.pred_state, st.state);
                }
                ec_gain_code_update(&mut st.ec_gain_c_st, bfi, st.prev_bf, &mut gain_code);
                pit_sharp = gain_pit;
            }
        }

        if mode != Mode::Mr475 || even_subfr == 0 {
            st.sharp = gain_pit;
            if st.sharp > SHARPMAX {
                st.sharp = SHARPMAX;
            }
        }

        pit_sharp = shl(pit_sharp, 1);
        if pit_sharp > 16384 {
            for i in 0..L_SUBFR {
                let temp = mult(st.old_exc[EXC_OFF + i], pit_sharp);
                let mut lt = l_mult(temp, gain_pit);
                if mode == Mode::Mr122 {
                    lt = if lt < 0 { !((!lt) >> 1) } else { lt >> 1 };
                }
                excp[i] = round_(lt);
            }
        }

        if bfi == 0 {
            for i in 0..8 {
                st.ltp_gain_history[i] = st.ltp_gain_history[i + 1];
            }
            st.ltp_gain_history[8] = gain_pit;
        }

        if (st.prev_bf != 0 || bfi != 0)
            && st.in_background_noise != 0
            && matches!(mode, Mode::Mr475 | Mode::Mr515 | Mode::Mr59)
        {
            if gain_pit > 12288 {
                gain_pit = ((gain_pit - 12288) >> 1) + 12288;
            }
            if gain_pit > 14745 {
                gain_pit = 14745;
            }
        }

        let past = st.lsf_state.past_lsf_q;
        int_lsf(&prev_lsf, &past, i_subfr as i16, &mut lsf_i);
        let mut gain_code_mix = cb_gain_average(
            &mut st.cb_gain_aver_state,
            mode,
            gain_code,
            &lsf_i,
            &st.lsp_avg_st.lsp_mean_save,
            bfi,
            st.prev_bf,
            pdfi,
            st.prev_pdf,
            st.in_background_noise,
            st.voiced_hangover,
        );
        if (mode as usize) > (Mode::Mr67 as usize) && mode != Mode::Mr102 {
            gain_code_mix = gain_code;
        }

        let (pitch_fac, tmp_shift): (i16, i16);
        if (mode as usize) <= (Mode::Mr102 as usize) {
            pitch_fac = gain_pit;
            tmp_shift = 1;
        } else {
            pitch_fac = if gain_pit < 0 { !((!gain_pit) >> 1) } else { gain_pit >> 1 };
            tmp_shift = 2;
        }

        for i in 0..L_SUBFR {
            exc_enhanced[i] = st.old_exc[EXC_OFF + i];
            let mut lt = l_mult(st.old_exc[EXC_OFF + i], pitch_fac);
            lt = l_mac(lt, code[i], gain_code);
            lt = l_shl(lt, tmp_shift);
            st.old_exc[EXC_OFF + i] = round_(lt);
        }

        ph_disp_release(&mut st.ph_disp_st);
        if matches!(mode, Mode::Mr475 | Mode::Mr515 | Mode::Mr59)
            && st.voiced_hangover > 3
            && st.in_background_noise != 0
            && bfi != 0
        {
            ph_disp_lock(&mut st.ph_disp_st);
        }

        ph_disp(
            &mut st.ph_disp_st,
            mode,
            &mut exc_enhanced,
            gain_code_mix,
            gain_pit,
            &mut code,
            pitch_fac,
            tmp_shift,
        );

        let mut lt: i32 = 0;
        for i in 0..L_SUBFR {
            lt = l_mac(lt, exc_enhanced[i], exc_enhanced[i]);
        }
        lt = if lt < 0 { !((!lt) >> 1) } else { lt >> 1 };
        let mut temp: i16 = 0;
        lt = sqrt_l_exp(lt, &mut temp);
        lt = l_shr(lt, (temp >> 1) + 15);
        let exc_energy: i16 = if lt < 0 { !((!lt) >> 2) } else { lt >> 2 } as i16;

        if matches!(mode, Mode::Mr475 | Mode::Mr515 | Mode::Mr59)
            && st.voiced_hangover > 5
            && st.in_background_noise != 0
            && st.state < 4
            && ((pdfi != 0 && st.prev_pdf != 0) || bfi != 0 || st.prev_bf != 0)
        {
            let careful_flag: i16 = (pdfi != 0 && bfi == 0) as i16;
            ex_ctrl(
                &mut exc_enhanced,
                exc_energy,
                &st.exc_energy_hist,
                st.voiced_hangover,
                st.prev_bf,
                careful_flag,
            );
        }

        if !(st.in_background_noise != 0 && (bfi != 0 || st.prev_bf != 0) && st.state < 4) {
            for i in 0..8 {
                st.exc_energy_hist[i] = st.exc_energy_hist[i + 1];
            }
            st.exc_energy_hist[8] = exc_energy;
        }

        let a = &a_t[az_off..az_off + MP1];
        if pit_sharp > 16384 {
            for i in 0..L_SUBFR {
                excp[i] = add(excp[i], exc_enhanced[i]);
            }
            agc2(&exc_enhanced, &mut excp);
            syn_filt(a, &excp, &mut synth[i_subfr..i_subfr + L_SUBFR], L_SUBFR, &mut st.mem_syn, true);
        } else {
            syn_filt(a, &exc_enhanced, &mut synth[i_subfr..i_subfr + L_SUBFR], L_SUBFR, &mut st.mem_syn, true);
        }

        st.old_exc.copy_within(L_SUBFR..L_SUBFR + EXC_OFF, 0);
        az_off += MP1;
        st.old_t0 = t0;
        i_subfr += L_SUBFR;
    }

    st.in_background_noise = bgn_scd(
        &mut st.background_state,
        &st.ltp_gain_history,
        synth,
        &mut st.voiced_hangover,
    );
    dtx_dec_activity_update(&mut st.dtx_decoder_state, &st.lsf_state.past_lsf_q, synth);
    st.prev_bf = bfi;
    st.prev_pdf = pdfi;
    lsp_avg(&mut st.lsp_avg_st, &st.lsf_state.past_lsf_q);
    st.dtx_decoder_state.dtx_global_state = new_dtx_state;
}

/// Full AMR-NB decoder (C `Speech_Decode_FrameState` + wrapper `prev_mode`).
#[derive(Clone, Debug)]
pub struct AmrNbDecoder {
    params: CodecParameters,
    dec: DecoderAmrState,
    post: PostFilterState,
    post_hp: PostProcessState,
    prev_mode: Mode,
    pending: Option<AudioFrame>,
    drained: bool,
}

impl AmrNbDecoder {
    pub fn new() -> Self {
        let mut params = CodecParameters::new(CodecId::AmrNb);
        params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: 8000,
            layout: ChannelLayout::Mono,
            format: Some(SampleFormat::S16),
            bits_per_sample: Some(16),
        });
        AmrNbDecoder {
            params,
            dec: DecoderAmrState::new(),
            post: PostFilterState::new(),
            post_hp: PostProcessState::new(),
            prev_mode: Mode::Mr475,
            pending: None,
            drained: false,
        }
    }

    /// Decoder for an AMR-NB track (`samr`); no extradata is needed.
    pub fn from_parameters(params: CodecParameters) -> Result<AmrNbDecoder> {
        let mut d = AmrNbDecoder::new();
        d.params.extradata = params.extradata;
        Ok(d)
    }

    /// Decode one frame (ToC byte + payload) into 160 S16 samples at 8 kHz. Truncated or
    /// invalid frames are decoded as lost frames (bad speech / no data); never panics.
    pub fn decode(&mut self, frame_with_toc: &[u8]) -> Result<[i16; 160]> {
        let mut prm = [0i16; MAX_PRM_SIZE + 1];
        let (frame_type, mode) = match parse_frame(frame_with_toc, self.prev_mode) {
            Ok(pf) => {
                prm = pf.prm;
                (pf.frame_type, pf.mode)
            }
            Err(BitstreamError::Truncated) => {
                let ft = (frame_with_toc.first().copied().unwrap_or(0xf8) >> 3) & 0x0f;
                if ft <= 7 {
                    (RxFrameType::SpeechBad, Mode::from_index(ft as usize).unwrap())
                } else if ft == 8 {
                    (RxFrameType::SidBad, self.prev_mode)
                } else {
                    (RxFrameType::NoData, self.prev_mode)
                }
            }
            Err(BitstreamError::InvalidFrameType) => (RxFrameType::NoData, self.prev_mode),
        };
        let mut synth = [0i16; L_FRAME];
        let mut az = [0i16; AZ_SIZE];
        decoder_amr(&mut self.dec, mode, &prm, frame_type, &mut synth, &mut az);
        post_filter(&mut self.post, mode, &mut synth, &az);
        post_process(&mut self.post_hp, &mut synth, L_FRAME);
        for s in synth.iter_mut() {
            *s &= !7;
        }
        self.prev_mode = mode;
        Ok(synth)
    }
}

impl Default for AmrNbDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for AmrNbDecoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        let pcm = self.decode(&packet.data)?;
        let mut bytes = Vec::with_capacity(320);
        for s in pcm {
            bytes.extend_from_slice(&s.to_ne_bytes());
        }
        let mut frame = AudioFrame::try_new(
            SampleFormat::S16,
            false,
            ChannelLayout::Mono,
            8000,
            L_FRAME,
            vec![Buf::from_vec(bytes)],
        )?;
        frame.pts = packet
            .pts
            .map(|ticks| ec_core::timebase::Timestamp::new(ticks, packet.time_base));
        self.pending = Some(frame);
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.pending.take() {
            Some(frame) => Ok(Frame::Audio(frame)),
            None if self.drained => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        self.drained = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.dec = DecoderAmrState::new();
        self.post = PostFilterState::new();
        self.post_hp = PostProcessState::new();
        self.prev_mode = Mode::Mr475;
        self.pending = None;
        self.drained = false;
    }
}

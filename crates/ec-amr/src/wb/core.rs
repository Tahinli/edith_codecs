//! Port of `pvamrwbdecoder.cpp`: the AMR-WB frame decoder (state + `pvDecoder_AmrWb`).

use super::bits::{Prms, Serial_parm, Serial_parm_1bit};
use super::dtx::*;
use super::exc::*;
use super::isf::*;
use super::math::extract_h;
use super::ops::*;
use super::synth::{SynthState, synthesis_amr_wb};

const INTERPOL_FRAC: [i16; NB_SUBFR] = [14746, 26214, 31457, 32767];

const ISP_INIT: [i16; M] = [
    32138, 30274, 27246, 23170, 18205, 12540, 6393, 0, -6393, -12540, -18205, -23170, -27246,
    -30274, -32138, 1475,
];
const ISF_INIT: [i16; M] = [
    1024, 2048, 3072, 4096, 5120, 6144, 7168, 8192, 9216, 10240, 11264, 12288, 13312, 14336, 15360,
    3840,
];

const OLD_EXC_LEN: usize = PIT_MAX as usize + L_INTERPOL;

/// Decoder state (`Decoder_State`).
pub struct DecoderState {
    pub old_exc: [i16; OLD_EXC_LEN],
    pub ispold: [i16; M],
    pub isfold: [i16; M],
    pub isf_buf: [i16; L_MEANBUF * M],
    pub past_isfq: [i16; M],
    pub tilt_code: i16,
    pub q_old: i16,
    pub qsubfr: [i16; 4],
    pub l_gc_thres: i32,
    pub synth: SynthState,
    pub seed: i16,
    pub old_t0: i16,
    pub old_t0_frac: i16,
    pub lag_hist: [i16; 5],
    pub dec_gain: [i16; 23],
    pub seed3: i16,
    pub disp_mem: [i16; 8],
    pub prev_bfi: i16,
    pub state: i16,
    pub first_frame: i16,
    pub dtx: DtxDecState,
}

impl DecoderState {
    /// `pvDecoder_AmrWb_Init`.
    pub fn new() -> Box<DecoderState> {
        let mut s = Box::new(DecoderState {
            old_exc: [0; OLD_EXC_LEN],
            ispold: [0; M],
            isfold: [0; M],
            isf_buf: [0; L_MEANBUF * M],
            past_isfq: [0; M],
            tilt_code: 0,
            q_old: Q_MAX,
            qsubfr: [Q_MAX; 4],
            l_gc_thres: 0,
            synth: SynthState::new(),
            seed: RANDOM_INITSEED,
            old_t0: 64,
            old_t0_frac: 0,
            lag_hist: [0; 5],
            dec_gain: [0; 23],
            seed3: RANDOM_INITSEED,
            disp_mem: [0; 8],
            prev_bfi: 0,
            state: 0,
            first_frame: 1,
            dtx: DtxDecState::default(),
        });
        dtx_dec_amr_wb_reset(&mut s.dtx, &ISF_INIT);
        s.reset(true);
        s
    }

    /// `pvDecoder_AmrWb_Reset`.
    pub fn reset(&mut self, reset_all: bool) {
        self.old_exc = [0; OLD_EXC_LEN];
        self.past_isfq = [0; M];
        self.old_t0_frac = 0;
        self.old_t0 = 64;
        self.first_frame = 1;
        self.l_gc_thres = 0;
        self.tilt_code = 0;
        self.disp_mem = [0; 8];
        self.q_old = Q_MAX;
        self.qsubfr = [Q_MAX; 4];
        if reset_all {
            dec_gain2_amr_wb_init(&mut self.dec_gain);
            self.synth.reset(true);
            init_lagconc(&mut self.lag_hist);
            self.ispold = ISP_INIT;
            self.isfold = ISF_INIT;
            for i in 0..L_MEANBUF {
                self.isf_buf[i * M..(i + 1) * M].copy_from_slice(&ISF_INIT);
            }
            self.synth.mem_deemph = 0;
            self.seed = RANDOM_INITSEED;
            self.synth.seed2 = RANDOM_INITSEED;
            self.seed3 = RANDOM_INITSEED;
            self.state = 0;
            self.prev_bfi = 0;
            self.synth.mem_syn_hf = [0; M16K];
            self.synth.mem_syn_hi = [0; M];
            self.synth.mem_syn_lo = [0; M];
            dtx_dec_amr_wb_reset(&mut self.dtx, &ISF_INIT);
            self.synth.vad_hist = 0;
        }
    }
}

/// `pvDecoder_AmrWb`: decode one frame of parameters (`prms`, one bit per i16 in
/// the decoder's serial order) into 320 samples.
pub fn decode_frame(
    mode: i16,
    prms_buf: &[i16],
    synth16k: &mut [i16],
    st: &mut DecoderState,
    frame_type: i16,
) {
    let mut prms = Prms::new(prms_buf);
    let nb_bits = AMR_WB_COMPRESSED[(mode & 15).min(9) as usize];

    let mut old_exc = [0i16; (L_FRAME + 1) + PIT_MAX as usize + L_INTERPOL];
    let mut aq = [0i16; NB_SUBFR * (M + 1)];
    let mut ispnew = [0i16; M];
    let mut isf = [0i16; M];
    let mut isf_tmp = [0i16; M];
    let mut code = [0i16; L_SUBFR];
    let mut excp = [0i16; L_SUBFR];
    let mut exc2 = [0i16; L_FRAME];
    let mut hf_isf = [0i16; M16K];
    let mut scratch = [0i16; 256];
    let eo = OLD_EXC_LEN; // index of exc[0] inside old_exc

    let new_dtx_state = rx_amr_wb_dtx_handler(&mut st.dtx, frame_type);
    if new_dtx_state != SPEECH {
        dtx_dec_amr_wb(&mut st.dtx, &mut exc2, new_dtx_state, &mut isf, &mut prms);
    }

    let (bfi, unusable_frame): (i16, i16);
    if frame_type == RX_SPEECH_BAD || frame_type == RX_SPEECH_PROBABLY_DEGRADED {
        bfi = 1;
        unusable_frame = 0;
    } else if frame_type == RX_NO_DATA || frame_type == RX_SPEECH_LOST {
        bfi = 1;
        unusable_frame = 1;
    } else {
        bfi = 0;
        unusable_frame = 0;
    }
    if bfi != 0 {
        st.state += 1;
        if st.state > 6 {
            st.state = 6;
        }
    } else {
        st.state >>= 1;
    }

    if st.dtx.dtxGlobalState == DTX {
        st.state = 5;
        st.prev_bfi = 0;
    } else if st.dtx.dtxGlobalState == DTX_MUTE {
        st.state = 5;
        st.prev_bfi = 1;
    }

    if new_dtx_state == SPEECH {
        let vad_flag = Serial_parm_1bit(&mut prms);
        if bfi == 0 {
            if vad_flag == 0 {
                st.synth.vad_hist = add_int16(st.synth.vad_hist, 1);
            } else {
                st.synth.vad_hist = 0;
            }
        }
    }

    // DTX-CNG
    if new_dtx_state != SPEECH {
        isf_isp(&isf, &mut ispnew, M as i16);
        isp_az(&ispnew, &mut aq, M as i16, 1);
        isf_tmp.copy_from_slice(&st.isfold);
        let mut i_subfr = 0;
        while i_subfr < L_FRAME {
            let j = i_subfr >> 6;
            for i in 0..M {
                let mut l_tmp = mul_16by16_to_int32(isf_tmp[i], sub_int16(32767, INTERPOL_FRAC[j]));
                l_tmp = mac_16by16_to_int32(l_tmp, isf[i], INTERPOL_FRAC[j]);
                hf_isf[i] = amr_wb_round(l_tmp);
            }
            let out = i_subfr * 5 / 4;
            synthesis_amr_wb(
                &aq,
                &mut exc2[i_subfr..i_subfr + L_SUBFR],
                0,
                &mut synth16k[out..out + L_SUBFR16K],
                1,
                &mut hf_isf,
                nb_bits,
                new_dtx_state,
                &mut st.synth,
                bfi,
            );
            i_subfr += L_SUBFR;
        }
        st.reset(false);
        st.isfold.copy_from_slice(&isf);
        st.prev_bfi = bfi;
        st.dtx.dtxGlobalState = new_dtx_state;
        return;
    }

    // ACELP
    old_exc[..OLD_EXC_LEN].copy_from_slice(&st.old_exc);
    let mut ind = [0i16; 8];

    if nb_bits > NBBITS_7K {
        ind[0] = Serial_parm(8, &mut prms);
        ind[1] = Serial_parm(8, &mut prms);
        ind[2] = Serial_parm(6, &mut prms);
        ind[3] = Serial_parm(7, &mut prms);
        ind[4] = Serial_parm(7, &mut prms);
        ind[5] = Serial_parm(5, &mut prms);
        ind[6] = Serial_parm(5, &mut prms);
        dpisf_2s_46b(
            &ind,
            &mut isf,
            &mut st.past_isfq,
            &st.isfold,
            &mut st.isf_buf,
            bfi,
            1,
        );
    } else {
        ind[0] = Serial_parm(8, &mut prms);
        ind[1] = Serial_parm(8, &mut prms);
        ind[2] = Serial_parm(14, &mut prms);
        ind[3] = ind[2] & 0x007F;
        ind[2] >>= 7;
        ind[4] = Serial_parm(6, &mut prms);
        dpisf_2s_36b(
            &ind,
            &mut isf,
            &mut st.past_isfq,
            &st.isfold,
            &mut st.isf_buf,
            bfi,
            1,
        );
    }

    isf_isp(&isf, &mut ispnew, M as i16);
    if st.first_frame != 0 {
        st.first_frame = 0;
        st.ispold.copy_from_slice(&ispnew);
    }
    interpolate_isp(&st.ispold, &ispnew, &INTERPOL_FRAC, &mut aq);
    st.ispold.copy_from_slice(&ispnew);

    // stability factor
    let mut l_tmp: i32 = 0;
    for i in 0..M - 1 {
        let tmp = sub_int16(isf[i], st.isfold[i]);
        l_tmp = mac_16by16_to_int32(l_tmp, tmp, tmp);
    }
    let mut tmp = extract_h(shl_int32(l_tmp, 8));
    tmp = mult_int16(tmp, 26214);
    tmp = (20480i32 - tmp as i32) as i16;
    let mut stab_fac = shl_int16(tmp, 1);
    if stab_fac < 0 {
        stab_fac = 0;
    }
    isf_tmp.copy_from_slice(&st.isfold);
    st.isfold.copy_from_slice(&isf);

    let mut t0: i16 = 0;
    let mut t0_frac: i16 = 0;
    let mut t0_min: i16 = 0;
    let mut t0_max: i16;
    let mut q_new: i16 = 0;
    let mut gain_pit: i16 = 0;
    let mut l_gain_code: i32 = 0;
    let mut p_aq = 0usize;

    let mut i_subfr = 0usize;
    while i_subfr < L_FRAME {
        let mut pit_flag = i_subfr as i16;
        if i_subfr == 2 * L_SUBFR && nb_bits > NBBITS_7K {
            pit_flag = 0;
        }
        let index: i16;
        if pit_flag == 0 {
            if nb_bits <= NBBITS_9K {
                index = Serial_parm(8, &mut prms);
                if index < (PIT_FR1_8B - PIT_MIN) * 2 {
                    t0 = PIT_MIN + (index >> 1);
                    t0_frac = sub_int16(index, shl_int16(sub_int16(t0, PIT_MIN), 1));
                    t0_frac = shl_int16(t0_frac, 1);
                } else {
                    t0 = add_int16(index, PIT_FR1_8B - ((PIT_FR1_8B - PIT_MIN) * 2));
                    t0_frac = 0;
                }
            } else {
                index = Serial_parm(9, &mut prms);
                if index < (PIT_FR2 - PIT_MIN) * 4 {
                    t0 = PIT_MIN + (index >> 2);
                    t0_frac = sub_int16(index, shl_int16(sub_int16(t0, PIT_MIN), 2));
                } else if index < (((PIT_FR2 - PIT_MIN) << 2) + ((PIT_FR1_9B - PIT_FR2) << 1)) {
                    let idx = index - ((PIT_FR2 - PIT_MIN) << 2);
                    t0 = PIT_FR2 + (idx >> 1);
                    t0_frac = sub_int16(idx, shl_int16(sub_int16(t0, PIT_FR2), 1));
                    t0_frac = shl_int16(t0_frac, 1);
                } else {
                    t0 = add_int16(
                        index,
                        PIT_FR1_9B - ((PIT_FR2 - PIT_MIN) * 4) - ((PIT_FR1_9B - PIT_FR2) * 2),
                    );
                    t0_frac = 0;
                }
            }
            t0_min = t0 - 8;
            if t0_min < PIT_MIN {
                t0_min = PIT_MIN;
            }
            t0_max = t0_min + 15;
            if t0_max > PIT_MAX {
                t0_min = PIT_MAX - 15;
            }
        } else if nb_bits <= NBBITS_9K {
            index = Serial_parm(5, &mut prms);
            t0 = t0_min + (index >> 1);
            t0_frac = sub_int16(index, shl_int16(t0 - t0_min, 1));
            t0_frac = shl_int16(t0_frac, 1);
        } else {
            index = Serial_parm(6, &mut prms);
            t0 = t0_min + (index >> 2);
            t0_frac = sub_int16(index, shl_int16(t0 - t0_min, 2));
        }

        if bfi != 0 {
            let old_t0 = st.old_t0;
            lagconceal(
                &st.dec_gain[17..],
                &st.lag_hist,
                &mut t0,
                &old_t0,
                &mut st.seed3,
                unusable_frame,
            );
            t0_frac = 0;
        }

        pred_lt4(
            &mut old_exc,
            eo + i_subfr,
            t0,
            t0_frac,
            (L_SUBFR + 1) as i16,
        );

        let select: i16 = if unusable_frame != 0 {
            1
        } else if nb_bits <= NBBITS_9K {
            0
        } else {
            Serial_parm_1bit(&mut prms)
        };

        if select == 0 {
            for i in 0..L_SUBFR {
                let base = eo + i_subfr + i;
                let mut l: i32 = old_exc[base - 1] as i32 + old_exc[base + 1] as i32;
                l = l.wrapping_mul(5898);
                l = l.wrapping_add((old_exc[base] as i32).wrapping_mul(20972));
                code[i] = amr_wb_round(l.wrapping_shl(1));
            }
            old_exc[eo + i_subfr..eo + i_subfr + L_SUBFR].copy_from_slice(&code);
        }

        // innovative codebook
        if unusable_frame != 0 {
            for c in code.iter_mut() {
                *c = noise_gen_amrwb(&mut st.seed) >> 3;
            }
        } else if nb_bits <= NBBITS_7K {
            ind[0] = Serial_parm(12, &mut prms);
            dec_acelp_2p_in_64(ind[0], &mut code);
        } else if nb_bits <= NBBITS_9K {
            for i in 0..4 {
                ind[i] = Serial_parm(5, &mut prms);
            }
            dec_acelp_4p_in_64(&ind, 20, &mut code);
        } else if nb_bits <= NBBITS_12K {
            for i in 0..4 {
                ind[i] = Serial_parm(9, &mut prms);
            }
            dec_acelp_4p_in_64(&ind, 36, &mut code);
        } else if nb_bits <= NBBITS_14K {
            ind[0] = Serial_parm(13, &mut prms);
            ind[1] = Serial_parm(13, &mut prms);
            ind[2] = Serial_parm(9, &mut prms);
            ind[3] = Serial_parm(9, &mut prms);
            dec_acelp_4p_in_64(&ind, 44, &mut code);
        } else if nb_bits <= NBBITS_16K {
            for i in 0..4 {
                ind[i] = Serial_parm(13, &mut prms);
            }
            dec_acelp_4p_in_64(&ind, 52, &mut code);
        } else if nb_bits <= NBBITS_18K {
            for i in 0..4 {
                ind[i] = Serial_parm(2, &mut prms);
            }
            for i in 4..8 {
                ind[i] = Serial_parm(14, &mut prms);
            }
            dec_acelp_4p_in_64(&ind, 64, &mut code);
        } else if nb_bits <= NBBITS_20K {
            ind[0] = Serial_parm(10, &mut prms);
            ind[1] = Serial_parm(10, &mut prms);
            ind[2] = Serial_parm(2, &mut prms);
            ind[3] = Serial_parm(2, &mut prms);
            ind[4] = Serial_parm(10, &mut prms);
            ind[5] = Serial_parm(10, &mut prms);
            ind[6] = Serial_parm(14, &mut prms);
            ind[7] = Serial_parm(14, &mut prms);
            dec_acelp_4p_in_64(&ind, 72, &mut code);
        } else {
            for i in 0..8 {
                ind[i] = Serial_parm(11, &mut prms);
            }
            dec_acelp_4p_in_64(&ind, 88, &mut code);
        }

        preemph_amrwb_dec(&mut code, st.tilt_code, L_SUBFR as i16);

        let mut tmp = t0;
        if t0_frac > 2 {
            tmp += 1;
        }
        pit_shrp(&mut code, tmp, PIT_SHARP, L_SUBFR as i16);

        // gains
        let (gidx, gbits) = if nb_bits <= NBBITS_9K {
            (Serial_parm(6, &mut prms), 6)
        } else {
            (Serial_parm(7, &mut prms), 7)
        };
        dec_gain2_amr_wb(
            gidx,
            gbits,
            &code,
            L_SUBFR as i16,
            &mut gain_pit,
            &mut l_gain_code,
            bfi,
            st.prev_bfi,
            st.state,
            unusable_frame,
            st.synth.vad_hist,
            &mut st.dec_gain,
        );
        // scaling of the excitation
        let mut tmp = st.qsubfr[0];
        for i in 1..4 {
            if st.qsubfr[i] < tmp {
                tmp = st.qsubfr[i];
            }
        }
        if tmp > Q_MAX {
            tmp = Q_MAX;
        }
        q_new = 0;
        let mut l_tmp = l_gain_code;
        while l_tmp < 0x0800_0000 && q_new < tmp {
            l_tmp = l_tmp.wrapping_shl(1);
            q_new += 1;
        }
        let mut gain_code = amr_wb_round(l_tmp);
        scale_signal(
            &mut old_exc[i_subfr..],
            (OLD_EXC_LEN + L_SUBFR) as i16,
            q_new - st.q_old,
        );
        st.q_old = q_new;

        if bfi == 0 {
            for i in (1..=4).rev() {
                st.lag_hist[i] = st.lag_hist[i - 1];
            }
            st.lag_hist[0] = t0;
            st.old_t0 = t0;
            st.old_t0_frac = 0;
        }

        // voice factor
        for i in (0..L_SUBFR).rev() {
            let e = old_exc[eo + i_subfr + i];
            exc2[i] = ((e as i32 + 4 * (e != MAX_16) as i32) >> 3) as i16;
        }

        let mut pit_sharp: i16;
        if nb_bits <= NBBITS_9K {
            pit_sharp = shl_int16(gain_pit, 1);
            if pit_sharp > 16384 {
                for i in 0..L_SUBFR {
                    let t = mult_int16(exc2[i], pit_sharp);
                    let mut l = mul_16by16_to_int32(t, gain_pit);
                    l >>= 1;
                    excp[i] = amr_wb_round(l);
                }
            }
        } else {
            pit_sharp = 0;
        }

        let voice_fac = voice_factor(&exc2, -3, gain_pit, &code, gain_code, L_SUBFR as i16);
        st.tilt_code = (voice_fac >> 2) + 8192;

        // total excitation
        exc2[..L_SUBFR].copy_from_slice(&old_exc[eo + i_subfr..eo + i_subfr + L_SUBFR]);
        let mut max: i32 = 1;
        for i in 0..L_SUBFR {
            let mut l = mul_16by16_to_int32(code[i], gain_code);
            l = shl_int32(l, 5);
            l = mac_16by16_to_int32(l, old_exc[eo + i_subfr + i], gain_pit);
            l = shl_int32(l, 1);
            let mut t = amr_wb_round(l);
            old_exc[eo + i_subfr + i] = t;
            t = (t as i32 - (t < 0) as i32) as i16;
            max |= (t ^ (t >> 15)) as i32;
        }
        let t = ((add_int16(normalize_amr_wb(max) - 16, q_new)) as i32 - 1) as i16;
        st.qsubfr[3] = st.qsubfr[2];
        st.qsubfr[2] = st.qsubfr[1];
        st.qsubfr[1] = st.qsubfr[0];
        st.qsubfr[0] = t;

        // phase dispersion
        let j: i16 = if nb_bits <= NBBITS_7K {
            0
        } else if nb_bits <= NBBITS_9K {
            1
        } else {
            2
        };
        phase_dispersion(
            (l_gain_code >> 16) as i16,
            gain_pit,
            &mut code,
            j,
            &mut st.disp_mem,
            &mut scratch,
        );

        // noise enhancer
        let t = 16384 - (voice_fac >> 1);
        let fac = mult_int16(stab_fac, t);
        let mut l_tmp = l_gain_code;
        if l_tmp < st.l_gc_thres {
            l_tmp = l_tmp.wrapping_add(fxp_mul32_by_16b(l_gain_code, 6226).wrapping_shl(1));
            if l_tmp > st.l_gc_thres {
                l_tmp = st.l_gc_thres;
            }
        } else {
            l_tmp = fxp_mul32_by_16b(l_gain_code, 27536).wrapping_shl(1);
            if l_tmp < st.l_gc_thres {
                l_tmp = st.l_gc_thres;
            }
        }
        st.l_gc_thres = l_tmp;
        l_gain_code = fxp_mul32_by_16b(l_gain_code, 32767 - fac as i32).wrapping_shl(1);
        l_gain_code = add_int32(
            l_gain_code,
            fxp_mul32_by_16b(l_tmp, fac as i32).wrapping_shl(1),
        );

        // pitch enhancer
        let tmp = (voice_fac >> 3) + 4096;
        gain_code = amr_wb_round(shl_int32(l_gain_code, q_new));

        let enh = |code: &[i16], i: usize, nxt: i16, prv: i16, e2: i16, gp: i16, gc: i16| -> i16 {
            let _ = i;
            let mut l = (code[i] as i32).wrapping_shl(16);
            l = msu_16by16_from_int32(l, nxt.wrapping_add(prv), tmp);
            l = mul_16by16_to_int32(amr_wb_round(l), gc);
            l = shl_int32(l, 5);
            l = mac_16by16_to_int32(l, e2, gp);
            l = shl_int32(l, 1);
            amr_wb_round(l)
        };
        exc2[0] = enh(&code, 0, code[1], 0, exc2[0], gain_pit, gain_code);
        for i in 1..L_SUBFR - 1 {
            exc2[i] = enh(
                &code,
                i,
                code[i + 1],
                code[i - 1],
                exc2[i],
                gain_pit,
                gain_code,
            );
        }
        exc2[L_SUBFR - 1] = enh(
            &code,
            L_SUBFR - 1,
            0,
            code[L_SUBFR - 2],
            exc2[L_SUBFR - 1],
            gain_pit,
            gain_code,
        );

        if nb_bits <= NBBITS_9K && pit_sharp > 16384 {
            for i in 0..L_SUBFR {
                excp[i] = add_int16(excp[i], exc2[i]);
            }
            agc2_amr_wb(&exc2[..L_SUBFR], &mut excp, L_SUBFR as i16);
            exc2[..L_SUBFR].copy_from_slice(&excp);
        }
        let _ = &mut pit_sharp;

        if nb_bits <= NBBITS_7K {
            let j = i_subfr >> 6;
            for i in 0..M {
                let mut l = mul_16by16_to_int32(isf_tmp[i], sub_int16(32767, INTERPOL_FRAC[j]));
                l = mac_16by16_to_int32(l, isf[i], INTERPOL_FRAC[j]);
                hf_isf[i] = amr_wb_round(l);
            }
        } else {
            for v in st.synth.mem_syn_hf[..M16K - M].iter_mut() {
                *v = 0;
            }
        }

        let corr_gain = if nb_bits >= NBBITS_24K {
            Serial_parm(4, &mut prms)
        } else {
            0
        };

        let out = i_subfr + (i_subfr >> 2);
        synthesis_amr_wb(
            &aq[p_aq..p_aq + M + 1],
            &mut exc2[..L_SUBFR],
            q_new,
            &mut synth16k[out..out + L_SUBFR16K],
            corr_gain,
            &mut hf_isf,
            nb_bits,
            new_dtx_state,
            &mut st.synth,
            bfi,
        );
        p_aq += M + 1;
        i_subfr += L_SUBFR;
    }

    st.old_exc
        .copy_from_slice(&old_exc[L_FRAME..L_FRAME + OLD_EXC_LEN]);
    scale_signal(&mut old_exc[eo..], L_FRAME as i16, -q_new);
    dtx_dec_amr_wb_activity_update(&mut st.dtx, &isf, &old_exc[eo..eo + L_FRAME]);
    st.dtx.dtxGlobalState = new_dtx_state;
    st.prev_bfi = bfi;
}

//! The SILK (linear-prediction) layer of Opus — RFC 6716 Section 4.2.
//!
//! SILK is a speech coder: each 20 ms frame carries an LPC filter (as line
//! spectral frequencies), a per-subframe gain, an optional long-term (pitch)
//! predictor, and a pulse-coded excitation. Decoding runs that chain backwards
//! — excitation, LTP synthesis, LPC synthesis — then unmixes stereo and
//! resamples from the internal 8/12/16 kHz to the output rate.
//!
//! Unlike CELT, SILK has no floating-point form: every operation below is the
//! exact integer arithmetic of the normative decoder (RFC 6716 Section 6 makes
//! the Appendix A reference implementation normative, and Section 4.2 is
//! written as fixed-point pseudo-code for the same reason). The tables are the
//! ones the RFC tabulates in Section 4.2, in the layout the reference uses.
//!
//! Packet loss concealment is not implemented: this decoder is fed complete
//! packets by the container layer, so a lost or empty frame yields silence
//! rather than an extrapolation. Everything else in the layer is complete.

// SILK is transliterated fixed-point arithmetic: the loops below index parallel
// arrays by position because the reference's index arithmetic is the
// specification. Iterator forms would hide that correspondence.
#![allow(clippy::needless_range_loop)]

use ec_core::Result;

use crate::range::RangeDecoder;

pub(crate) mod tables {
    #![allow(clippy::all)]
    include!("silk_tables.rs");
}

use tables::*;

pub(crate) const MAX_LPC_ORDER: usize = 16;
pub(crate) const LTP_ORDER: usize = 5;
pub(crate) const MAX_NB_SUBFR: usize = 4;
const SUB_FRAME_LENGTH_MS: usize = 5;
const LTP_MEM_LENGTH_MS: usize = 20;
const MAX_FRAME_LENGTH: usize = 20 * 16;
const QUANT_LEVEL_ADJUST_Q10: i32 = 80;
const N_LEVELS_QGAIN: i32 = 64;
const MIN_DELTA_GAIN_QUANT: i32 = -4;
const MAX_DELTA_GAIN_QUANT: i32 = 36;
const MIN_QGAIN_DB: i32 = 2;
const MAX_QGAIN_DB: i32 = 88;
pub(crate) const NLSF_QUANT_MAX_AMPLITUDE: i32 = 4;
const SHELL_LEN: usize = 16;
const MAX_PULSES: i32 = 16;
const N_RATE_LEVELS: usize = 10;
pub(crate) const PE_MIN_LAG_MS: i32 = 2;
pub(crate) const PE_MAX_LAG_MS: i32 = 18;
const STEREO_INTERP_LEN_MS: usize = 8;
const TYPE_VOICED: i32 = 2;
const MAX_LPC_STABILIZE_ITERATIONS: usize = 16;

/// Gain quantiser constants (`gain_quant.c`).
const GAIN_OFFSET: i32 = (MIN_QGAIN_DB * 128) / 6 + 16 * 128;
const GAIN_INV_SCALE_Q16: i32 =
    (65536 * (((MAX_QGAIN_DB - MIN_QGAIN_DB) * 128) / 6)) / (N_LEVELS_QGAIN - 1);

/// How a frame's parameters relate to the previous frame's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CondCoding {
    Independently,
    IndependentlyNoLtpScaling,
    Conditionally,
}

// ---------------------------------------------------------------------------
// Fixed-point helpers. These are the reference's macros; they are written out
// because SILK's output is defined by them bit for bit.
// ---------------------------------------------------------------------------

#[inline]
pub(crate) fn smulwb(a: i32, b: i32) -> i32 {
    (((a >> 16) * (b as i16 as i32)) as i64 + ((((a & 0xFFFF) * (b as i16 as i32)) >> 16) as i64))
        as i32
}

#[inline]
pub(crate) fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulwb(b, c))
}

#[inline]
pub(crate) fn smulbb(a: i32, b: i32) -> i32 {
    (a as i16 as i32).wrapping_mul(b as i16 as i32)
}

#[inline]
fn smlabb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulbb(b, c))
}

#[inline]
pub(crate) fn rshift_round(a: i32, shift: u32) -> i32 {
    if shift == 1 {
        (a >> 1) + (a & 1)
    } else {
        ((a >> (shift - 1)) + 1) >> 1
    }
}

#[inline]
fn rshift_round64(a: i64, shift: u32) -> i64 {
    if shift == 1 {
        (a >> 1) + (a & 1)
    } else {
        ((a >> (shift - 1)) + 1) >> 1
    }
}

#[inline]
fn smulww(a: i32, b: i32) -> i32 {
    smulwb(a, b).wrapping_add(a.wrapping_mul(rshift_round(b, 16)))
}

#[inline]
fn smlaww(a: i32, b: i32, c: i32) -> i32 {
    smlawb(a, b, c).wrapping_add(b.wrapping_mul(rshift_round(c, 16)))
}

/// `silk_SMMUL`: the top 32 bits of a 32x32 product.
#[inline]
fn smmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}

#[inline]
fn sat16(a: i32) -> i16 {
    a.clamp(-32768, 32767) as i16
}

#[inline]
fn clz32(x: i32) -> i32 {
    if x == 0 {
        32
    } else {
        (x as u32).leading_zeros() as i32
    }
}

/// `silk_CLZ_FRAC`: leading zeros plus the seven bits below the leading one.
#[inline]
fn clz_frac(x: i32) -> (i32, i32) {
    let lz = clz32(x);
    let frac = (x as u32).rotate_right((24 - lz) as u32) as i32 & 0x7F;
    (lz, frac)
}

pub(crate) fn sqrt_approx(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    let (lz, frac) = clz_frac(x);
    let mut y = if lz & 1 != 0 { 32768 } else { 46214 };
    y >>= lz >> 1;
    smlawb(y, y, smulbb(213, frac))
}

pub(crate) fn log2lin(log_q7: i32) -> i32 {
    if log_q7 < 0 {
        return 0;
    }
    let mut out = 1i32 << (log_q7 >> 7);
    let frac = log_q7 & 0x7F;
    let adj = smlawb(frac, smulbb(frac, 128 - frac), -174);
    if log_q7 < 2048 {
        out += ((out as i64 * adj as i64) >> 7) as i32;
    } else {
        out = out.wrapping_add((out >> 7).wrapping_mul(adj));
    }
    out
}

/// `silk_INVERSE32_varQ`: `(1 << q) / b`, to about 30 bits.
fn inverse32_var_q(b32: i32, qres: u32) -> i32 {
    debug_assert!(b32 != 0);
    let b_headrm = clz32(b32.abs()) - 1;
    let b32_nrm = b32 << b_headrm;
    let b32_inv = (i32::MAX >> 2) / (b32_nrm >> 16);
    let mut result = b32_inv << 16;
    let err_q32 = ((1 << 29) - smulwb(b32_nrm, b32_inv)) << 3;
    result = smlaww(result, err_q32, b32_inv);
    let lshift = 61 - b_headrm - qres as i32;
    if lshift <= 0 {
        lshift_sat32(result, -lshift)
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}

/// `silk_DIV32_varQ`: `(a << q) / b`.
fn div32_var_q(a32: i32, b32: i32, qres: u32) -> i32 {
    debug_assert!(b32 != 0);
    let a_headrm = clz32(a32.abs()) - 1;
    let mut a32_nrm = a32 << a_headrm;
    let b_headrm = clz32(b32.abs()) - 1;
    let b32_nrm = b32 << b_headrm;
    let b32_inv = (i32::MAX >> 2) / (b32_nrm >> 16);
    let mut result = smulwb(a32_nrm, b32_inv);
    a32_nrm = a32_nrm.wrapping_sub(smmul(b32_nrm, result).wrapping_shl(3));
    result = smlawb(result, a32_nrm, b32_inv);
    let lshift = 29 + a_headrm - b_headrm - qres as i32;
    if lshift < 0 {
        lshift_sat32(result, -lshift)
    } else if lshift < 32 {
        result >> lshift
    } else {
        0
    }
}

fn lshift_sat32(a: i32, shift: i32) -> i32 {
    let lim = i32::MAX >> shift;
    a.clamp(-lim - 1, lim) << shift
}

/// `silk_RAND`: the excitation sign PRNG.
#[inline]
pub(crate) fn silk_rand(seed: i32) -> i32 {
    (907633515i32).wrapping_add(seed.wrapping_mul(196314165))
}

// ---------------------------------------------------------------------------
// Decoder state
// ---------------------------------------------------------------------------

/// Per-frame parameters decoded from the bitstream.
#[derive(Clone, Debug, Default)]
struct Indices {
    signal_type: i32,
    quant_offset_type: i32,
    gains: [i8; MAX_NB_SUBFR],
    nlsf: [i8; MAX_LPC_ORDER + 1],
    nlsf_interp_coef_q2: i32,
    lag_index: i32,
    contour_index: i32,
    per_index: usize,
    ltp_index: [usize; MAX_NB_SUBFR],
    ltp_scale_index: usize,
    seed: i32,
}

/// Everything `decode_parameters` produces for one frame.
#[derive(Clone, Debug)]
struct FrameCtrl {
    gains_q16: [i32; MAX_NB_SUBFR],
    pred_coef_q12: [[i16; MAX_LPC_ORDER]; 2],
    ltp_coef_q14: [i16; LTP_ORDER * MAX_NB_SUBFR],
    ltp_scale_q14: i32,
    pitch_l: [i32; MAX_NB_SUBFR],
}

impl Default for FrameCtrl {
    fn default() -> Self {
        FrameCtrl {
            gains_q16: [0; MAX_NB_SUBFR],
            pred_coef_q12: [[0; MAX_LPC_ORDER]; 2],
            ltp_coef_q14: [0; LTP_ORDER * MAX_NB_SUBFR],
            ltp_scale_q14: 0,
            pitch_l: [0; MAX_NB_SUBFR],
        }
    }
}

/// One SILK channel's inter-frame state.
#[derive(Clone, Debug)]
struct ChannelState {
    fs_khz: usize,
    frame_length: usize,
    subfr_length: usize,
    ltp_mem_length: usize,
    nb_subfr: usize,
    lpc_order: usize,
    frames_per_packet: usize,
    frames_decoded: usize,
    /// Wideband uses the 16th-order codebook, NB/MB the 10th-order one.
    wb_codebook: bool,
    indices: Indices,
    /// Synthesis history, `ltp_mem_length + frame_length` samples.
    out_buf: [i16; MAX_FRAME_LENGTH + MAX_FRAME_LENGTH],
    s_lpc_q14: [i32; MAX_LPC_ORDER],
    exc_q14: [i32; MAX_FRAME_LENGTH],
    prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    prev_gain_q16: i32,
    last_gain_index: i32,
    prev_signal_type: i32,
    ec_prev_signal_type: i32,
    ec_prev_lag_index: i32,
    lag_prev: i32,
    first_frame_after_reset: bool,
    vad_flags: [bool; 3],
    lbrr_flag: bool,
    lbrr_flags: [bool; 3],
    resampler: Resampler,
}

impl ChannelState {
    fn new() -> ChannelState {
        ChannelState {
            fs_khz: 0,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            nb_subfr: 0,
            lpc_order: 0,
            frames_per_packet: 1,
            frames_decoded: 0,
            wb_codebook: false,
            indices: Indices::default(),
            out_buf: [0; MAX_FRAME_LENGTH * 2],
            s_lpc_q14: [0; MAX_LPC_ORDER],
            exc_q14: [0; MAX_FRAME_LENGTH],
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            prev_gain_q16: 65536,
            last_gain_index: 10,
            prev_signal_type: 0,
            ec_prev_signal_type: 0,
            ec_prev_lag_index: 0,
            lag_prev: 100,
            first_frame_after_reset: true,
            vad_flags: [false; 3],
            lbrr_flag: false,
            lbrr_flags: [false; 3],
            resampler: Resampler::default(),
        }
    }

    /// `silk_decoder_set_fs`: geometry for an internal rate.
    fn set_fs(&mut self, fs_khz: usize, api_rate: u32) {
        self.subfr_length = SUB_FRAME_LENGTH_MS * fs_khz;
        let frame_length = self.nb_subfr * self.subfr_length;
        if self.fs_khz != fs_khz || self.resampler.fs_out_khz * 1000 != api_rate as usize {
            self.resampler = Resampler::new(fs_khz, api_rate as usize / 1000);
        }
        if self.fs_khz != fs_khz {
            self.ltp_mem_length = LTP_MEM_LENGTH_MS * fs_khz;
            if fs_khz == 16 {
                self.lpc_order = MAX_LPC_ORDER;
                self.wb_codebook = true;
            } else {
                self.lpc_order = 10;
                self.wb_codebook = false;
            }
            self.first_frame_after_reset = true;
            self.lag_prev = 100;
            self.last_gain_index = 10;
            self.prev_signal_type = 0;
            self.out_buf.fill(0);
            self.s_lpc_q14.fill(0);
        }
        self.fs_khz = fs_khz;
        self.frame_length = frame_length;
    }

    fn pitch_contour_icdf(&self) -> &'static [u8] {
        match (self.fs_khz, self.nb_subfr) {
            (8, 4) => &PITCH_CONTOUR_NB_ICDF,
            (8, _) => &PITCH_CONTOUR_10_MS_NB_ICDF,
            (_, 4) => &PITCH_CONTOUR_ICDF,
            _ => &PITCH_CONTOUR_10_MS_ICDF,
        }
    }

    fn pitch_lag_low_bits_icdf(&self) -> &'static [u8] {
        match self.fs_khz {
            16 => &UNIFORM8_ICDF,
            12 => &UNIFORM6_ICDF,
            _ => &UNIFORM4_ICDF,
        }
    }
}

/// Stereo prediction state carried between frames.
#[derive(Clone, Debug, Default)]
struct StereoState {
    pred_prev_q13: [i32; 2],
    s_mid: [i16; 2],
    s_side: [i16; 2],
}

/// The SILK decoder for one Opus stream (one or two channels).
#[derive(Clone, Debug)]
pub struct SilkDecoder {
    channels: [ChannelState; 2],
    stereo: StereoState,
    channels_internal: usize,
    channels_api: usize,
    api_rate: u32,
    prev_decode_only_middle: bool,
    /// Resampler output for one channel, allocated once.
    resample_tmp: Vec<i16>,
}

impl SilkDecoder {
    /// A decoder producing `channels_api` channels at `api_rate` Hz.
    pub fn new(api_rate: u32, channels_api: usize) -> SilkDecoder {
        SilkDecoder {
            channels: [ChannelState::new(), ChannelState::new()],
            stereo: StereoState::default(),
            channels_internal: 1,
            channels_api,
            api_rate,
            prev_decode_only_middle: false,
            resample_tmp: vec![0; 5760],
        }
    }

    /// The last 20 ms of decoded output of channel 0 at the internal rate:
    /// the synthesis history `decode_core` whitens for its LTP state. The
    /// encoder mirrors its own packets through a `SilkDecoder` and reads this
    /// so its LTP search runs on exactly what the real decoder will predict
    /// from.
    pub(crate) fn history(&self) -> &[i16] {
        let ch = &self.channels[0];
        &ch.out_buf[..ch.ltp_mem_length]
    }

    /// Drops all inter-packet state.
    pub fn reset(&mut self) {
        let api_rate = self.api_rate;
        let channels_api = self.channels_api;
        *self = SilkDecoder::new(api_rate, channels_api);
    }

    /// Decodes the SILK part of one Opus frame.
    ///
    /// `payload_ms` is the Opus frame duration (10, 20, 40 or 60 ms),
    /// `internal_rate` the SILK sample rate the TOC implies (8000, 12000 or
    /// 16000 Hz), and `channels_internal` how many channels the packet codes.
    /// Output is interleaved `i16` at the API rate, `channels_api` channels.
    pub fn decode(
        &mut self,
        dec: &mut RangeDecoder,
        out: &mut [i16],
        payload_ms: usize,
        internal_rate: u32,
        channels_internal: usize,
        new_packet: bool,
    ) -> Result<usize> {
        let fs_khz = (internal_rate / 1000) as usize;
        if new_packet {
            for ch in self.channels.iter_mut() {
                ch.frames_decoded = 0;
            }
        }
        let (frames_per_packet, nb_subfr) = match payload_ms {
            10 => (1, 2),
            20 => (1, 4),
            40 => (2, 4),
            60 => (3, 4),
            _ => (1, 4),
        };
        // A stream that gains a second channel starts it from scratch.
        if channels_internal > self.channels_internal {
            self.channels[1] = ChannelState::new();
        }
        if self.channels[0].frames_decoded == 0 {
            for ch in self.channels.iter_mut().take(channels_internal) {
                ch.frames_per_packet = frames_per_packet;
                ch.nb_subfr = nb_subfr;
                ch.set_fs(fs_khz, self.api_rate);
            }
        }
        if self.channels_api == 2 && channels_internal == 2 && self.channels_internal == 1 {
            // The side channel starts from the mid channel's resampler state,
            // so the two stay in phase across the mono-to-stereo switch.
            self.stereo.pred_prev_q13 = [0; 2];
            self.stereo.s_side = [0; 2];
            self.channels[1].resampler = self.channels[0].resampler.clone();
        }
        self.channels_internal = channels_internal;

        let mut ms_pred_q13 = [0i32; 2];
        let mut decode_only_middle = false;

        // Header bits: per-frame VAD flags and the LBRR flag (Section 4.2.3).
        if self.channels[0].frames_decoded == 0 {
            for n in 0..channels_internal {
                for i in 0..self.channels[n].frames_per_packet {
                    self.channels[n].vad_flags[i] = dec.dec_bit_logp(1);
                }
                self.channels[n].lbrr_flag = dec.dec_bit_logp(1);
            }
            for n in 0..channels_internal {
                self.channels[n].lbrr_flags = [false; 3];
                if self.channels[n].lbrr_flag {
                    if self.channels[n].frames_per_packet == 1 {
                        self.channels[n].lbrr_flags[0] = true;
                    } else {
                        let icdf: &[u8] = if self.channels[n].frames_per_packet == 2 {
                            &LBRR_FLAGS_2_ICDF
                        } else {
                            &LBRR_FLAGS_3_ICDF
                        };
                        let sym = dec.dec_icdf(icdf, 8) as i32 + 1;
                        for i in 0..self.channels[n].frames_per_packet {
                            self.channels[n].lbrr_flags[i] = (sym >> i) & 1 != 0;
                        }
                    }
                }
            }
            // Skip the LBRR frames: they are only useful for concealment, but
            // their symbols must still be consumed to stay in sync.
            for i in 0..self.channels[0].frames_per_packet {
                for n in 0..channels_internal {
                    if self.channels[n].lbrr_flags[i] {
                        if channels_internal == 2 && n == 0 {
                            stereo_decode_pred(dec, &mut ms_pred_q13);
                            if !self.channels[1].lbrr_flags[i] {
                                let _ = dec.dec_icdf(&STEREO_ONLY_CODE_MID_ICDF, 8);
                            }
                        }
                        let cond = if i > 0 && self.channels[n].lbrr_flags[i - 1] {
                            CondCoding::Conditionally
                        } else {
                            CondCoding::Independently
                        };
                        let ch = &mut self.channels[n];
                        decode_indices(ch, dec, i, true, cond);
                        let mut pulses = [0i32; MAX_FRAME_LENGTH + SHELL_LEN];
                        let n = ch.frame_length;
                        decode_pulses(
                            dec,
                            &mut pulses,
                            n,
                            ch.indices.signal_type,
                            ch.indices.quant_offset_type,
                        );
                    }
                }
            }
        }

        // Stereo prediction weights for this frame.
        if channels_internal == 2 {
            stereo_decode_pred(dec, &mut ms_pred_q13);
            let frame = self.channels[0].frames_decoded;
            if !self.channels[1].vad_flags[frame] {
                decode_only_middle = dec.dec_icdf(&STEREO_ONLY_CODE_MID_ICDF, 8) != 0;
            }
        }
        if channels_internal == 2 && !decode_only_middle && self.prev_decode_only_middle {
            self.channels[1].out_buf.fill(0);
            self.channels[1].s_lpc_q14.fill(0);
            self.channels[1].lag_prev = 100;
            self.channels[1].last_gain_index = 10;
            self.channels[1].prev_signal_type = 0;
            self.channels[1].first_frame_after_reset = true;
        }

        let has_side = !decode_only_middle;
        let mut mid = [0i16; MAX_FRAME_LENGTH + 2];
        let mut side = [0i16; MAX_FRAME_LENGTH + 2];
        let mut n_samples_dec = 0usize;
        for n in 0..channels_internal {
            let buf: &mut [i16] = if n == 0 { &mut mid } else { &mut side };
            if n == 0 || has_side {
                let frame_index = self.channels[0].frames_decoded as i32 - n as i32;
                let cond = if frame_index <= 0 {
                    CondCoding::Independently
                } else if n > 0 && self.prev_decode_only_middle {
                    CondCoding::IndependentlyNoLtpScaling
                } else {
                    CondCoding::Conditionally
                };
                n_samples_dec = decode_frame(&mut self.channels[n], dec, &mut buf[2..], cond);
            } else {
                buf[2..2 + n_samples_dec].fill(0);
            }
            self.channels[n].frames_decoded += 1;
        }

        // Stereo unmixing, then resampling to the output rate.
        if self.channels_api == 2 && channels_internal == 2 {
            stereo_ms_to_lr(
                &mut self.stereo,
                &mut mid,
                &mut side,
                &ms_pred_q13,
                self.channels[0].fs_khz,
                n_samples_dec,
            );
        } else {
            mid[0] = self.stereo.s_mid[0];
            mid[1] = self.stereo.s_mid[1];
            self.stereo.s_mid[0] = mid[n_samples_dec];
            self.stereo.s_mid[1] = mid[n_samples_dec + 1];
        }
        self.prev_decode_only_middle = decode_only_middle;

        let out_len = n_samples_dec * self.api_rate as usize / (self.channels[0].fs_khz * 1000);
        let mut tmp = core::mem::take(&mut self.resample_tmp);
        tmp.clear();
        tmp.resize(out_len, 0);
        let chans = self.channels_api.min(channels_internal);
        for n in 0..chans {
            let src: &[i16] = if n == 0 { &mid[1..] } else { &side[1..] };
            let state = &mut self.channels[n].resampler;
            state.process(&mut tmp, &src[..n_samples_dec], n_samples_dec);
            if self.channels_api == 2 {
                for i in 0..out_len {
                    out[n + 2 * i] = tmp[i];
                }
            } else {
                out[..out_len].copy_from_slice(&tmp[..out_len]);
            }
        }
        self.resample_tmp = tmp;
        // A mono stream fed to a stereo output is duplicated.
        if self.channels_api == 2 && channels_internal == 1 {
            for i in 0..out_len {
                out[1 + 2 * i] = out[2 * i];
            }
        }
        Ok(out_len)
    }
}

/// `silk_decode_indices` (Section 4.2.7).
fn decode_indices(
    ch: &mut ChannelState,
    dec: &mut RangeDecoder,
    frame_index: usize,
    decode_lbrr: bool,
    cond: CondCoding,
) {
    let ix = if decode_lbrr || ch.vad_flags[frame_index] {
        dec.dec_icdf(&TYPE_OFFSET_VAD_ICDF, 8) as i32 + 2
    } else {
        dec.dec_icdf(&TYPE_OFFSET_NO_VAD_ICDF, 8) as i32
    };
    ch.indices.signal_type = ix >> 1;
    ch.indices.quant_offset_type = ix & 1;

    // Subframe gains (Section 4.2.7.4).
    if cond == CondCoding::Conditionally {
        ch.indices.gains[0] = dec.dec_icdf(&DELTA_GAIN_ICDF, 8) as i8;
    } else {
        let msb = dec.dec_icdf(&GAIN_ICDF[ch.indices.signal_type as usize], 8) as i32;
        let lsb = dec.dec_icdf(&UNIFORM8_ICDF, 8) as i32;
        ch.indices.gains[0] = ((msb << 3) + lsb) as i8;
    }
    for i in 1..ch.nb_subfr {
        ch.indices.gains[i] = dec.dec_icdf(&DELTA_GAIN_ICDF, 8) as i8;
    }

    // Normalised LSFs (Section 4.2.7.5).
    let cb = nlsf_cb(ch.wb_codebook);
    let stage1_icdf = &cb.cb1_icdf[(ch.indices.signal_type as usize >> 1) * cb.n_vectors..];
    ch.indices.nlsf[0] = dec.dec_icdf(stage1_icdf, 8) as i8;
    let (ec_ix, _pred) = nlsf_unpack(cb, ch.indices.nlsf[0] as usize);
    for i in 0..cb.order {
        let mut ix = dec.dec_icdf(&cb.ec_icdf[ec_ix[i] as usize..], 8) as i32;
        if ix == 0 {
            ix -= dec.dec_icdf(&NLSF_EXT_ICDF, 8) as i32;
        } else if ix == 2 * NLSF_QUANT_MAX_AMPLITUDE {
            ix += dec.dec_icdf(&NLSF_EXT_ICDF, 8) as i32;
        }
        ch.indices.nlsf[i + 1] = (ix - NLSF_QUANT_MAX_AMPLITUDE) as i8;
    }
    ch.indices.nlsf_interp_coef_q2 = if ch.nb_subfr == MAX_NB_SUBFR {
        dec.dec_icdf(&NLSF_INTERPOLATION_FACTOR_ICDF, 8) as i32
    } else {
        4
    };

    // Pitch and LTP (Section 4.2.7.6).
    if ch.indices.signal_type == TYPE_VOICED {
        let mut absolute = true;
        if cond == CondCoding::Conditionally && ch.ec_prev_signal_type == TYPE_VOICED {
            let delta = dec.dec_icdf(&PITCH_DELTA_ICDF, 8) as i32;
            if delta > 0 {
                ch.indices.lag_index = ch.ec_prev_lag_index + delta - 9;
                absolute = false;
            }
        }
        if absolute {
            ch.indices.lag_index =
                dec.dec_icdf(&PITCH_LAG_ICDF, 8) as i32 * (ch.fs_khz as i32 >> 1);
            ch.indices.lag_index += dec.dec_icdf(ch.pitch_lag_low_bits_icdf(), 8) as i32;
        }
        ch.ec_prev_lag_index = ch.indices.lag_index;
        ch.indices.contour_index = dec.dec_icdf(ch.pitch_contour_icdf(), 8) as i32;
        ch.indices.per_index = dec.dec_icdf(&LTP_PER_INDEX_ICDF, 8);
        let gain_icdf: &[u8] = match ch.indices.per_index {
            0 => &LTP_GAIN_ICDF_0,
            1 => &LTP_GAIN_ICDF_1,
            _ => &LTP_GAIN_ICDF_2,
        };
        for k in 0..ch.nb_subfr {
            ch.indices.ltp_index[k] = dec.dec_icdf(gain_icdf, 8);
        }
        ch.indices.ltp_scale_index = if cond == CondCoding::Independently {
            dec.dec_icdf(&LTPSCALE_ICDF, 8)
        } else {
            0
        };
    }
    ch.ec_prev_signal_type = ch.indices.signal_type;
    ch.indices.seed = dec.dec_icdf(&UNIFORM4_ICDF, 8) as i32;
}

/// `silk_decode_pulses` (Section 4.2.7.8): the excitation.
fn decode_pulses(
    dec: &mut RangeDecoder,
    pulses: &mut [i32],
    frame_length: usize,
    signal_type: i32,
    quant_offset_type: i32,
) {
    // A 12 kHz 10 ms frame is 120 samples, which is not a whole number of
    // 16-sample shell blocks: the last block runs past the frame and its extra
    // pulses are discarded, so `pulses` must have room for them.
    debug_assert!(pulses.len() >= frame_length.div_ceil(SHELL_LEN) * SHELL_LEN);
    let rate_level = dec.dec_icdf(&RATE_LEVELS_ICDF[signal_type as usize >> 1], 8);
    let mut iter = frame_length >> 4;
    if iter * SHELL_LEN < frame_length {
        iter += 1;
    }
    let mut sum_pulses = [0i32; MAX_FRAME_LENGTH / SHELL_LEN + 1];
    let mut n_lshifts = [0i32; MAX_FRAME_LENGTH / SHELL_LEN + 1];
    for i in 0..iter {
        sum_pulses[i] = dec.dec_icdf(&PULSES_PER_BLOCK_ICDF[rate_level], 8) as i32;
        while sum_pulses[i] == MAX_PULSES + 1 {
            n_lshifts[i] += 1;
            let icdf = &PULSES_PER_BLOCK_ICDF[N_RATE_LEVELS - 1][usize::from(n_lshifts[i] == 10)..];
            sum_pulses[i] = dec.dec_icdf(icdf, 8) as i32;
        }
    }
    for i in 0..iter {
        let block = &mut pulses[i * SHELL_LEN..];
        if sum_pulses[i] > 0 {
            shell_decode(dec, block, sum_pulses[i]);
        } else {
            block[..SHELL_LEN].fill(0);
        }
    }
    // Least significant bits for blocks that overflowed the pulse count.
    for i in 0..iter {
        if n_lshifts[i] > 0 {
            let block = &mut pulses[i * SHELL_LEN..];
            for k in 0..SHELL_LEN {
                let mut abs_q = block[k];
                for _ in 0..n_lshifts[i] {
                    abs_q <<= 1;
                    abs_q += dec.dec_icdf(&LSB_ICDF, 8) as i32;
                }
                block[k] = abs_q;
            }
            sum_pulses[i] |= n_lshifts[i] << 5;
        }
    }
    decode_signs(
        dec,
        pulses,
        signal_type,
        quant_offset_type,
        &sum_pulses[..iter],
    );
}

/// One 16-sample shell block (Section 4.2.7.8.3).
fn shell_decode(dec: &mut RangeDecoder, pulses0: &mut [i32], total: i32) {
    fn split(dec: &mut RangeDecoder, p: i32, table: &[u8]) -> (i32, i32) {
        if p > 0 {
            let a = dec.dec_icdf(&table[SHELL_CODE_TABLE_OFFSETS[p as usize] as usize..], 8) as i32;
            (a, p - a)
        } else {
            (0, 0)
        }
    }
    let mut p3 = [0i32; 2];
    let mut p2 = [0i32; 4];
    let mut p1 = [0i32; 8];
    (p3[0], p3[1]) = split(dec, total, &SHELL_CODE_TABLE3);
    (p2[0], p2[1]) = split(dec, p3[0], &SHELL_CODE_TABLE2);
    (p1[0], p1[1]) = split(dec, p2[0], &SHELL_CODE_TABLE1);
    (pulses0[0], pulses0[1]) = split(dec, p1[0], &SHELL_CODE_TABLE0);
    (pulses0[2], pulses0[3]) = split(dec, p1[1], &SHELL_CODE_TABLE0);
    (p1[2], p1[3]) = split(dec, p2[1], &SHELL_CODE_TABLE1);
    (pulses0[4], pulses0[5]) = split(dec, p1[2], &SHELL_CODE_TABLE0);
    (pulses0[6], pulses0[7]) = split(dec, p1[3], &SHELL_CODE_TABLE0);
    (p2[2], p2[3]) = split(dec, p3[1], &SHELL_CODE_TABLE2);
    (p1[4], p1[5]) = split(dec, p2[2], &SHELL_CODE_TABLE1);
    (pulses0[8], pulses0[9]) = split(dec, p1[4], &SHELL_CODE_TABLE0);
    (pulses0[10], pulses0[11]) = split(dec, p1[5], &SHELL_CODE_TABLE0);
    (p1[6], p1[7]) = split(dec, p2[3], &SHELL_CODE_TABLE1);
    (pulses0[12], pulses0[13]) = split(dec, p1[6], &SHELL_CODE_TABLE0);
    (pulses0[14], pulses0[15]) = split(dec, p1[7], &SHELL_CODE_TABLE0);
}

/// Excitation signs (Section 4.2.7.8.5).
fn decode_signs(
    dec: &mut RangeDecoder,
    pulses: &mut [i32],
    signal_type: i32,
    quant_offset_type: i32,
    sum_pulses: &[i32],
) {
    let base = 7 * (quant_offset_type + (signal_type << 1)) as usize;
    for (i, &p) in sum_pulses.iter().enumerate() {
        if p > 0 {
            let icdf = [SIGN_ICDF[base + (p & 0x1F).min(6) as usize], 0];
            for j in 0..SHELL_LEN {
                let idx = i * SHELL_LEN + j;
                if idx < pulses.len() && pulses[idx] > 0 {
                    pulses[idx] *= 2 * dec.dec_icdf(&icdf, 8) as i32 - 1;
                }
            }
        }
    }
}

/// The NLSF codebook for one bandwidth (Section 4.2.7.5).
pub(crate) struct NlsfCodebook {
    pub(crate) n_vectors: usize,
    pub(crate) order: usize,
    pub(crate) quant_step_size_q16: i32,
    pub(crate) cb1_q8: &'static [u8],
    pub(crate) cb1_icdf: &'static [u8],
    pub(crate) pred_q8: &'static [u8],
    pub(crate) ec_sel: &'static [u8],
    pub(crate) ec_icdf: &'static [u8],
    pub(crate) delta_min_q15: &'static [i16],
}

pub(crate) fn nlsf_cb(wideband: bool) -> &'static NlsfCodebook {
    static NB_MB: NlsfCodebook = NlsfCodebook {
        n_vectors: 32,
        order: 10,
        // 0.18 in Q16.
        quant_step_size_q16: 11796,
        cb1_q8: &NLSF_CB1_NB_MB_Q8,
        cb1_icdf: &NLSF_CB1_ICDF_NB_MB,
        pred_q8: &NLSF_PRED_NB_MB_Q8,
        ec_sel: &NLSF_CB2_SELECT_NB_MB,
        ec_icdf: &NLSF_CB2_ICDF_NB_MB,
        delta_min_q15: &NLSF_DELTA_MIN_NB_MB_Q15,
    };
    static WB: NlsfCodebook = NlsfCodebook {
        n_vectors: 32,
        order: 16,
        quant_step_size_q16: 9830,
        cb1_q8: &NLSF_CB1_WB_Q8,
        cb1_icdf: &NLSF_CB1_ICDF_WB,
        pred_q8: &NLSF_PRED_WB_Q8,
        ec_sel: &NLSF_CB2_SELECT_WB,
        ec_icdf: &NLSF_CB2_ICDF_WB,
        delta_min_q15: &NLSF_DELTA_MIN_WB_Q15,
    };
    if wideband { &WB } else { &NB_MB }
}

/// `silk_NLSF_unpack`: per-coefficient entropy-table selectors and predictors.
pub(crate) fn nlsf_unpack(cb: &NlsfCodebook, cb1_index: usize) -> ([i16; MAX_LPC_ORDER], [u8; MAX_LPC_ORDER]) {
    let mut ec_ix = [0i16; MAX_LPC_ORDER];
    let mut pred = [0u8; MAX_LPC_ORDER];
    let sel = &cb.ec_sel[cb1_index * cb.order / 2..];
    for i in (0..cb.order).step_by(2) {
        let entry = sel[i / 2];
        ec_ix[i] = (((entry >> 1) & 7) as i16) * (2 * NLSF_QUANT_MAX_AMPLITUDE as i16 + 1);
        pred[i] = cb.pred_q8[i + (entry & 1) as usize * (cb.order - 1)];
        ec_ix[i + 1] = (((entry >> 5) & 7) as i16) * (2 * NLSF_QUANT_MAX_AMPLITUDE as i16 + 1);
        pred[i + 1] = cb.pred_q8[i + ((entry >> 4) & 1) as usize * (cb.order - 1) + 1];
    }
    (ec_ix, pred)
}

/// `silk_NLSF_decode`: indices to normalised LSFs (Section 4.2.7.5.3).
pub(crate) fn nlsf_decode(indices: &[i8], cb: &NlsfCodebook) -> [i16; MAX_LPC_ORDER] {
    let mut nlsf_q15 = [0i16; MAX_LPC_ORDER];
    let base = indices[0] as usize * cb.order;
    for i in 0..cb.order {
        nlsf_q15[i] = (cb.cb1_q8[base + i] as i16) << 7;
    }
    let (_, pred_q8) = nlsf_unpack(cb, indices[0] as usize);
    // Residual dequantisation, back to front so the prediction chains.
    let mut res_q10 = [0i16; MAX_LPC_ORDER];
    let mut out_q10 = 0i32;
    for i in (0..cb.order).rev() {
        let pred_q10 = smulbb(out_q10, pred_q8[i] as i32) >> 8;
        out_q10 = (indices[i + 1] as i32) << 10;
        if out_q10 > 0 {
            out_q10 -= 102; // 0.1 in Q10
        } else if out_q10 < 0 {
            out_q10 += 102;
        }
        out_q10 = smlawb(pred_q10, out_q10, cb.quant_step_size_q16);
        res_q10[i] = out_q10 as i16;
    }
    // Weight by the Laroia measure, add, then enforce the minimum spacing.
    let w_qw = nlsf_vq_weights_laroia(&nlsf_q15[..cb.order]);
    for i in 0..cb.order {
        let w_q9 = sqrt_approx((w_qw[i] as i32) << 16);
        let v = nlsf_q15[i] as i32 + (((res_q10[i] as i32) << 14) / w_q9);
        nlsf_q15[i] = v.clamp(0, 32767) as i16;
    }
    nlsf_stabilize(&mut nlsf_q15[..cb.order], cb.delta_min_q15);
    nlsf_q15
}

/// `silk_NLSF_VQ_weights_laroia` (Section 4.2.7.5.3).
pub(crate) fn nlsf_vq_weights_laroia(nlsf_q15: &[i16]) -> [i16; MAX_LPC_ORDER] {
    let d = nlsf_q15.len();
    let mut w = [0i16; MAX_LPC_ORDER];
    let inv = |x: i32| (1i32 << 17) / x.max(1);
    let mut tmp1 = inv(nlsf_q15[0] as i32);
    let mut tmp2 = inv(nlsf_q15[1] as i32 - nlsf_q15[0] as i32);
    w[0] = (tmp1 + tmp2).min(32767) as i16;
    let mut k = 1;
    while k < d - 1 {
        tmp1 = inv(nlsf_q15[k + 1] as i32 - nlsf_q15[k] as i32);
        w[k] = (tmp1 + tmp2).min(32767) as i16;
        tmp2 = inv(nlsf_q15[k + 2] as i32 - nlsf_q15[k + 1] as i32);
        w[k + 1] = (tmp1 + tmp2).min(32767) as i16;
        k += 2;
    }
    tmp1 = inv((1 << 15) - nlsf_q15[d - 1] as i32);
    w[d - 1] = (tmp1 + tmp2).min(32767) as i16;
    w
}

/// `silk_NLSF_stabilize` (Section 4.2.7.5.4): enforce the minimum spacing.
pub(crate) fn nlsf_stabilize(nlsf_q15: &mut [i16], delta_min_q15: &[i16]) {
    let l = nlsf_q15.len();
    for _ in 0..20 {
        let mut min_diff = nlsf_q15[0] as i32 - delta_min_q15[0] as i32;
        let mut i_min = 0usize;
        for i in 1..l {
            let diff = nlsf_q15[i] as i32 - (nlsf_q15[i - 1] as i32 + delta_min_q15[i] as i32);
            if diff < min_diff {
                min_diff = diff;
                i_min = i;
            }
        }
        let diff = (1 << 15) - (nlsf_q15[l - 1] as i32 + delta_min_q15[l] as i32);
        if diff < min_diff {
            min_diff = diff;
            i_min = l;
        }
        if min_diff >= 0 {
            return;
        }
        if i_min == 0 {
            nlsf_q15[0] = delta_min_q15[0];
        } else if i_min == l {
            nlsf_q15[l - 1] = ((1 << 15) - delta_min_q15[l] as i32) as i16;
        } else {
            let mut min_center = 0i32;
            for k in 0..i_min {
                min_center += delta_min_q15[k] as i32;
            }
            min_center += delta_min_q15[i_min] as i32 >> 1;
            let mut max_center = 1i32 << 15;
            for k in (i_min + 1..=l).rev() {
                max_center -= delta_min_q15[k] as i32;
            }
            max_center -= delta_min_q15[i_min] as i32 >> 1;
            let center = rshift_round(nlsf_q15[i_min - 1] as i32 + nlsf_q15[i_min] as i32, 1)
                .clamp(min_center, max_center);
            nlsf_q15[i_min - 1] = (center - (delta_min_q15[i_min] as i32 >> 1)) as i16;
            nlsf_q15[i_min] = nlsf_q15[i_min - 1] + delta_min_q15[i_min];
        }
    }
    // Last resort: sort and clamp (the reference's fallback after 20 loops).
    nlsf_q15.sort_unstable();
    nlsf_q15[0] = nlsf_q15[0].max(delta_min_q15[0]);
    for i in 1..l {
        nlsf_q15[i] = nlsf_q15[i].max(nlsf_q15[i - 1].saturating_add(delta_min_q15[i]));
    }
    nlsf_q15[l - 1] = nlsf_q15[l - 1].min(((1 << 15) - delta_min_q15[l] as i32) as i16);
    for i in (0..l - 1).rev() {
        nlsf_q15[i] = nlsf_q15[i].min(nlsf_q15[i + 1] - delta_min_q15[i + 1]);
    }
}

/// `silk_NLSF2A` (Section 4.2.7.5.6): LSFs to LPC coefficients.
pub(crate) fn nlsf2a(nlsf: &[i16], order: usize) -> [i16; MAX_LPC_ORDER] {
    const QA: u32 = 16;
    const ORDER16: [usize; 16] = [0, 15, 8, 7, 4, 11, 12, 3, 2, 13, 10, 5, 6, 9, 14, 1];
    const ORDER10: [usize; 10] = [0, 9, 6, 3, 4, 5, 8, 1, 2, 7];
    let ordering: &[usize] = if order == 16 { &ORDER16 } else { &ORDER10 };

    let mut cos_lsf_qa = [0i32; MAX_LPC_ORDER];
    for k in 0..order {
        let f_int = (nlsf[k] >> 8) as usize;
        let f_frac = nlsf[k] as i32 - ((f_int as i32) << 8);
        let cos_val = LSFCOSTAB_FIX_Q12[f_int] as i32;
        let delta = LSFCOSTAB_FIX_Q12[f_int + 1] as i32 - cos_val;
        cos_lsf_qa[ordering[k]] = rshift_round((cos_val << 8) + delta * f_frac, 20 - QA);
    }
    let dd = order / 2;
    let p = nlsf2a_find_poly(&cos_lsf_qa, dd, 0);
    let q = nlsf2a_find_poly(&cos_lsf_qa, dd, 1);

    let mut a32_qa1 = [0i32; MAX_LPC_ORDER];
    for k in 0..dd {
        let ptmp = p[k + 1] + p[k];
        let qtmp = q[k + 1] - q[k];
        a32_qa1[k] = -qtmp - ptmp;
        a32_qa1[order - k - 1] = qtmp - ptmp;
    }

    // Clamp into Q12 without letting the filter blow up.
    let mut a_q12 = [0i16; MAX_LPC_ORDER];
    let mut i = 0;
    while i < 10 {
        let mut maxabs = 0i32;
        let mut idx = 0usize;
        for k in 0..order {
            let absval = a32_qa1[k].abs();
            if absval > maxabs {
                maxabs = absval;
                idx = k;
            }
        }
        let maxabs_q12 = rshift_round(maxabs, QA + 1 - 12);
        if maxabs_q12 > 32767 {
            let maxabs_q12 = maxabs_q12.min(163838);
            let sc_q16 =
                65471 - ((maxabs_q12 - 32767) << 14) / ((maxabs_q12 * (idx as i32 + 1)) >> 2);
            bwexpander_32(&mut a32_qa1[..order], sc_q16);
        } else {
            break;
        }
        i += 1;
    }
    if i == 10 {
        for k in 0..order {
            a_q12[k] = sat16(rshift_round(a32_qa1[k], QA + 1 - 12));
            a32_qa1[k] = (a_q12[k] as i32) << (QA + 1 - 12);
        }
    } else {
        for k in 0..order {
            a_q12[k] = rshift_round(a32_qa1[k], QA + 1 - 12) as i16;
        }
    }
    // Bandwidth expansion until the prediction gain is sane.
    for i in 0..MAX_LPC_STABILIZE_ITERATIONS {
        if lpc_inverse_pred_gain(&a_q12[..order]) < 107374 {
            bwexpander_32(&mut a32_qa1[..order], 65536 - (2 << i));
            for k in 0..order {
                a_q12[k] = rshift_round(a32_qa1[k], QA + 1 - 12) as i16;
            }
        } else {
            break;
        }
    }
    a_q12
}

fn nlsf2a_find_poly(cos_lsf_qa: &[i32], dd: usize, offset: usize) -> [i32; MAX_LPC_ORDER / 2 + 1] {
    const QA: u32 = 16;
    let mut out = [0i32; MAX_LPC_ORDER / 2 + 1];
    out[0] = 1 << QA;
    out[1] = -cos_lsf_qa[offset];
    for k in 1..dd {
        let ftmp = cos_lsf_qa[2 * k + offset];
        out[k + 1] = (out[k - 1] << 1) - rshift_round64(ftmp as i64 * out[k] as i64, QA) as i32;
        for n in (2..=k).rev() {
            out[n] += out[n - 2] - rshift_round64(ftmp as i64 * out[n - 1] as i64, QA) as i32;
        }
        out[1] -= ftmp;
    }
    out
}

fn bwexpander_32(ar: &mut [i32], chirp_q16: i32) {
    let mut chirp = chirp_q16;
    let chirp_minus_one = chirp_q16 - 65536;
    let d = ar.len();
    for coef in ar.iter_mut().take(d - 1) {
        *coef = smulww(chirp, *coef);
        chirp += rshift_round(chirp.wrapping_mul(chirp_minus_one), 16);
    }
    ar[d - 1] = smulww(chirp, ar[d - 1]);
}

/// `silk_LPC_inverse_pred_gain` (Section 4.2.7.5.8), in Q30.
fn lpc_inverse_pred_gain(a_q12: &[i16]) -> i32 {
    const QA: u32 = 24;
    const A_LIMIT: i32 = 16773022; // 0.99975 in Q24
    let order = a_q12.len();
    let mut a = [[0i32; MAX_LPC_ORDER]; 2];
    let mut dc_resp = 0i32;
    for k in 0..order {
        dc_resp += a_q12[k] as i32;
        a[order & 1][k] = (a_q12[k] as i32) << (QA - 12);
    }
    if dc_resp >= 4096 {
        return 0;
    }
    let mut cur = order & 1;
    let mut inv_gain_q30 = 1i32 << 30;
    for k in (1..order).rev() {
        if a[cur][k] > A_LIMIT || a[cur][k] < -A_LIMIT {
            return 0;
        }
        let rc_q31 = -(a[cur][k] << (31 - QA));
        let rc_mult1_q30 = (1i32 << 30) - smmul(rc_q31, rc_q31);
        let mult2q = 32 - clz32(rc_mult1_q30.abs());
        let rc_mult2 = inverse32_var_q(rc_mult1_q30, (mult2q + 30) as u32);
        inv_gain_q30 = smmul(inv_gain_q30, rc_mult1_q30) << 2;
        let old = cur;
        cur = k & 1;
        for n in 0..k {
            let tmp = a[old][n] - mul32_frac_q(a[old][k - n - 1], rc_q31, 31);
            a[cur][n] = mul32_frac_q(tmp, rc_mult2, mult2q as u32);
        }
    }
    if a[cur][0] > A_LIMIT || a[cur][0] < -A_LIMIT {
        return 0;
    }
    let rc_q31 = -(a[cur][0] << (31 - QA));
    let rc_mult1_q30 = (1i32 << 30) - smmul(rc_q31, rc_q31);
    smmul(inv_gain_q30, rc_mult1_q30) << 2
}

#[inline]
fn mul32_frac_q(a: i32, b: i32, q: u32) -> i32 {
    rshift_round64(a as i64 * b as i64, q) as i32
}

/// `silk_gains_dequant` (Section 4.2.7.4).
pub(crate) fn gains_dequant(
    indices: &[i8],
    prev_ind: &mut i32,
    conditional: bool,
    nb_subfr: usize,
) -> [i32; MAX_NB_SUBFR] {
    let mut gains = [0i32; MAX_NB_SUBFR];
    for k in 0..nb_subfr {
        if k == 0 && !conditional {
            // A gain may not drop more than 16 steps below the last one.
            *prev_ind = (indices[k] as i32).max(*prev_ind - 16);
        } else {
            let ind_tmp = indices[k] as i32 + MIN_DELTA_GAIN_QUANT;
            let threshold = 2 * MAX_DELTA_GAIN_QUANT - N_LEVELS_QGAIN + *prev_ind;
            if ind_tmp > threshold {
                *prev_ind += (ind_tmp << 1) - threshold;
            } else {
                *prev_ind += ind_tmp;
            }
        }
        *prev_ind = (*prev_ind).clamp(0, N_LEVELS_QGAIN - 1);
        gains[k] = log2lin((smulwb(GAIN_INV_SCALE_Q16, *prev_ind) + GAIN_OFFSET).min(3967));
    }
    gains
}

/// `silk_decode_pitch` (Section 4.2.7.6.1).
pub(crate) fn decode_pitch(
    lag_index: i32,
    contour_index: i32,
    fs_khz: i32,
    nb_subfr: usize,
) -> [i32; MAX_NB_SUBFR] {
    let mut pitch = [0i32; MAX_NB_SUBFR];
    let min_lag = PE_MIN_LAG_MS * fs_khz;
    let max_lag = PE_MAX_LAG_MS * fs_khz;
    let lag = min_lag + lag_index;
    for k in 0..nb_subfr {
        let offset = if fs_khz == 8 {
            if nb_subfr == 4 {
                CB_LAGS_STAGE2[k][contour_index as usize]
            } else {
                CB_LAGS_STAGE2_10_MS[k][contour_index as usize]
            }
        } else if nb_subfr == 4 {
            CB_LAGS_STAGE3[k][contour_index as usize]
        } else {
            CB_LAGS_STAGE3_10_MS[k][contour_index as usize]
        };
        pitch[k] = (lag + offset as i32).clamp(min_lag, max_lag);
    }
    pitch
}

/// `silk_decode_parameters`.
fn decode_parameters(ch: &mut ChannelState, cond: CondCoding) -> FrameCtrl {
    let mut ctrl = FrameCtrl {
        gains_q16: gains_dequant(
            &ch.indices.gains,
            &mut ch.last_gain_index,
            cond == CondCoding::Conditionally,
            ch.nb_subfr,
        ),
        ..Default::default()
    };
    let cb = nlsf_cb(ch.wb_codebook);
    let nlsf_q15 = nlsf_decode(&ch.indices.nlsf, cb);
    ctrl.pred_coef_q12[1] = nlsf2a(&nlsf_q15[..ch.lpc_order], ch.lpc_order);

    if ch.first_frame_after_reset {
        ch.indices.nlsf_interp_coef_q2 = 4;
    }
    if ch.indices.nlsf_interp_coef_q2 < 4 {
        // Interpolate the first half of the frame from the previous LSFs.
        let mut nlsf0 = [0i16; MAX_LPC_ORDER];
        for i in 0..ch.lpc_order {
            nlsf0[i] = (ch.prev_nlsf_q15[i] as i32
                + ((ch.indices.nlsf_interp_coef_q2
                    * (nlsf_q15[i] as i32 - ch.prev_nlsf_q15[i] as i32))
                    >> 2)) as i16;
        }
        ctrl.pred_coef_q12[0] = nlsf2a(&nlsf0[..ch.lpc_order], ch.lpc_order);
    } else {
        ctrl.pred_coef_q12[0] = ctrl.pred_coef_q12[1];
    }
    ch.prev_nlsf_q15[..ch.lpc_order].copy_from_slice(&nlsf_q15[..ch.lpc_order]);

    if ch.indices.signal_type == TYPE_VOICED {
        ctrl.pitch_l = decode_pitch(
            ch.indices.lag_index,
            ch.indices.contour_index,
            ch.fs_khz as i32,
            ch.nb_subfr,
        );
        let cbk: &[[i8; 5]] = match ch.indices.per_index {
            0 => &LTP_GAIN_VQ_0,
            1 => &LTP_GAIN_VQ_1,
            _ => &LTP_GAIN_VQ_2,
        };
        for k in 0..ch.nb_subfr {
            let v = &cbk[ch.indices.ltp_index[k]];
            for i in 0..LTP_ORDER {
                ctrl.ltp_coef_q14[k * LTP_ORDER + i] = (v[i] as i16) << 7;
            }
        }
        ctrl.ltp_scale_q14 = LTPSCALES_TABLE_Q14[ch.indices.ltp_scale_index] as i32;
    }
    ctrl
}

/// `silk_LPC_analysis_filter`: the whitening filter used to rebuild the LTP
/// state from previously decoded output.
fn lpc_analysis_filter(out: &mut [i16], input: &[i16], b_q12: &[i16], len: usize, d: usize) {
    for ix in d..len {
        let mut acc = 0i32;
        for j in 0..d {
            acc = acc.wrapping_add(smulbb(input[ix - 1 - j] as i32, b_q12[j] as i32));
        }
        let v = ((input[ix] as i32) << 12).wrapping_sub(acc);
        out[ix] = sat16(rshift_round(v, 12));
    }
    out[..d].fill(0);
}

/// `silk_decode_core` (Section 4.2.7.9): excitation to output samples.
fn decode_core(ch: &mut ChannelState, ctrl: &FrameCtrl, xq: &mut [i16], pulses: &[i32]) {
    let offset_q10 = QUANTIZATION_OFFSETS_Q10[ch.indices.signal_type as usize >> 1]
        [ch.indices.quant_offset_type as usize] as i32;
    let nlsf_interpolated = ch.indices.nlsf_interp_coef_q2 < 4;

    // Excitation: pulses with a quantisation offset and a pseudo-random sign.
    let mut rand_seed = ch.indices.seed;
    for i in 0..ch.frame_length {
        rand_seed = silk_rand(rand_seed);
        let mut e = pulses[i] << 14;
        if e > 0 {
            e -= QUANT_LEVEL_ADJUST_Q10 << 4;
        } else if e < 0 {
            e += QUANT_LEVEL_ADJUST_Q10 << 4;
        }
        e += offset_q10 << 4;
        ch.exc_q14[i] = if rand_seed < 0 { -e } else { e };
        rand_seed = rand_seed.wrapping_add(pulses[i]);
    }

    let mut s_lpc_q14 = [0i32; MAX_LPC_ORDER + 5 * 16];
    s_lpc_q14[..MAX_LPC_ORDER].copy_from_slice(&ch.s_lpc_q14);
    let mut s_ltp = [0i16; MAX_FRAME_LENGTH];
    let mut s_ltp_q15 = [0i32; 2 * MAX_FRAME_LENGTH];
    let mut sltp_buf_idx = ch.ltp_mem_length;
    let mut lag = 0usize;
    let mut res_q14 = [0i32; 5 * 16];

    for k in 0..ch.nb_subfr {
        let a_q12 = &ctrl.pred_coef_q12[k >> 1];
        let b_q14 = &ctrl.ltp_coef_q14[k * LTP_ORDER..k * LTP_ORDER + LTP_ORDER];
        let gain_q10 = ctrl.gains_q16[k] >> 6;
        let mut inv_gain_q31 = inverse32_var_q(ctrl.gains_q16[k], 47);
        let gain_adj_q16 = if ctrl.gains_q16[k] != ch.prev_gain_q16 {
            let adj = div32_var_q(ch.prev_gain_q16, ctrl.gains_q16[k], 16);
            for v in s_lpc_q14.iter_mut().take(MAX_LPC_ORDER) {
                *v = smulww(adj, *v);
            }
            adj
        } else {
            1 << 16
        };
        ch.prev_gain_q16 = ctrl.gains_q16[k];

        if ch.indices.signal_type == TYPE_VOICED {
            lag = ctrl.pitch_l[k] as usize;
            if k == 0 || (k == 2 && nlsf_interpolated) {
                // Rebuild the LTP history by whitening past output.
                let start_idx = ch.ltp_mem_length - lag - ch.lpc_order - LTP_ORDER / 2;
                if k == 2 {
                    let n = 2 * ch.subfr_length;
                    let base = ch.ltp_mem_length;
                    ch.out_buf[base..base + n].copy_from_slice(&xq[..n]);
                }
                lpc_analysis_filter(
                    &mut s_ltp[start_idx..],
                    &ch.out_buf[start_idx + k * ch.subfr_length..],
                    a_q12,
                    ch.ltp_mem_length - start_idx,
                    ch.lpc_order,
                );
                if k == 0 {
                    inv_gain_q31 = smulwb(inv_gain_q31, ctrl.ltp_scale_q14) << 2;
                }
                for i in 0..lag + LTP_ORDER / 2 {
                    s_ltp_q15[sltp_buf_idx - i - 1] =
                        smulwb(inv_gain_q31, s_ltp[ch.ltp_mem_length - i - 1] as i32);
                }
            } else if gain_adj_q16 != 1 << 16 {
                for i in 0..lag + LTP_ORDER / 2 {
                    s_ltp_q15[sltp_buf_idx - i - 1] =
                        smulww(gain_adj_q16, s_ltp_q15[sltp_buf_idx - i - 1]);
                }
            }
        }

        // Long-term prediction.
        let exc_base = k * ch.subfr_length;
        let pres_q14: &[i32] = if ch.indices.signal_type == TYPE_VOICED {
            let pred_base = sltp_buf_idx - lag + LTP_ORDER / 2;
            for i in 0..ch.subfr_length {
                let pred_idx = pred_base + i;
                let mut ltp_pred_q13 = 2i32;
                for (j, &b) in b_q14.iter().enumerate() {
                    ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[pred_idx - j], b as i32);
                }
                res_q14[i] = ch.exc_q14[exc_base + i].wrapping_add(ltp_pred_q13 << 1);
                s_ltp_q15[sltp_buf_idx] = res_q14[i] << 1;
                sltp_buf_idx += 1;
            }
            &res_q14[..ch.subfr_length]
        } else {
            &ch.exc_q14[exc_base..exc_base + ch.subfr_length]
        };

        // Short-term (LPC) synthesis.
        for i in 0..ch.subfr_length {
            let mut lpc_pred_q10 = (ch.lpc_order >> 1) as i32;
            for j in 0..ch.lpc_order {
                lpc_pred_q10 = smlawb(
                    lpc_pred_q10,
                    s_lpc_q14[MAX_LPC_ORDER + i - 1 - j],
                    a_q12[j] as i32,
                );
            }
            s_lpc_q14[MAX_LPC_ORDER + i] = pres_q14[i].wrapping_add(lpc_pred_q10 << 4);
            xq[k * ch.subfr_length + i] = sat16(rshift_round(
                smulww(s_lpc_q14[MAX_LPC_ORDER + i], gain_q10),
                8,
            ));
        }
        s_lpc_q14.copy_within(ch.subfr_length..ch.subfr_length + MAX_LPC_ORDER, 0);
    }
    ch.s_lpc_q14.copy_from_slice(&s_lpc_q14[..MAX_LPC_ORDER]);
}

/// `silk_decode_frame`: one SILK frame of one channel.
fn decode_frame(
    ch: &mut ChannelState,
    dec: &mut RangeDecoder,
    out: &mut [i16],
    cond: CondCoding,
) -> usize {
    let frame_index = ch.frames_decoded;
    decode_indices(ch, dec, frame_index, false, cond);
    let mut pulses = [0i32; MAX_FRAME_LENGTH + SHELL_LEN];
    decode_pulses(
        dec,
        &mut pulses,
        ch.frame_length,
        ch.indices.signal_type,
        ch.indices.quant_offset_type,
    );
    let mut ctrl = decode_parameters(ch, cond);
    if cond == CondCoding::IndependentlyNoLtpScaling {
        ctrl.ltp_scale_q14 = LTPSCALES_TABLE_Q14[0] as i32;
    }
    decode_core(ch, &ctrl, out, &pulses);
    ch.prev_signal_type = ch.indices.signal_type;
    ch.first_frame_after_reset = false;
    ch.lag_prev = ctrl.pitch_l[ch.nb_subfr - 1];

    // Slide the synthesis history.
    let mv_len = ch.ltp_mem_length - ch.frame_length;
    ch.out_buf
        .copy_within(ch.frame_length..ch.frame_length + mv_len, 0);
    ch.out_buf[mv_len..mv_len + ch.frame_length].copy_from_slice(&out[..ch.frame_length]);
    ch.frame_length
}

/// `silk_stereo_decode_pred` (Section 4.2.7.1).
fn stereo_decode_pred(dec: &mut RangeDecoder, pred_q13: &mut [i32; 2]) {
    let n = dec.dec_icdf(&STEREO_PRED_JOINT_ICDF, 8) as i32;
    let mut ix = [[0i32; 3]; 2];
    ix[0][2] = n / 5;
    ix[1][2] = n - 5 * ix[0][2];
    for row in ix.iter_mut() {
        row[0] = dec.dec_icdf(&UNIFORM3_ICDF, 8) as i32;
        row[1] = dec.dec_icdf(&UNIFORM5_ICDF, 8) as i32;
    }
    for n in 0..2 {
        let i0 = (ix[n][0] + 3 * ix[n][2]) as usize;
        let low = STEREO_PRED_QUANT_Q13[i0] as i32;
        // 0.5/5 in Q16.
        let step = smulwb(STEREO_PRED_QUANT_Q13[i0 + 1] as i32 - low, 6554);
        pred_q13[n] = smlabb(low, step, 2 * ix[n][1] + 1);
    }
    pred_q13[0] -= pred_q13[1];
}

/// `silk_stereo_MS_to_LR` (Section 4.2.8): mid/side back to left/right.
fn stereo_ms_to_lr(
    state: &mut StereoState,
    x1: &mut [i16],
    x2: &mut [i16],
    pred_q13: &[i32; 2],
    fs_khz: usize,
    frame_length: usize,
) {
    x1[0] = state.s_mid[0];
    x1[1] = state.s_mid[1];
    x2[0] = state.s_side[0];
    x2[1] = state.s_side[1];
    state.s_mid[0] = x1[frame_length];
    state.s_mid[1] = x1[frame_length + 1];
    state.s_side[0] = x2[frame_length];
    state.s_side[1] = x2[frame_length + 1];

    let interp = STEREO_INTERP_LEN_MS * fs_khz;
    let mut pred0 = state.pred_prev_q13[0];
    let mut pred1 = state.pred_prev_q13[1];
    let denom_q16 = (1i32 << 16) / interp as i32;
    let delta0 = rshift_round(smulbb(pred_q13[0] - state.pred_prev_q13[0], denom_q16), 16);
    let delta1 = rshift_round(smulbb(pred_q13[1] - state.pred_prev_q13[1], denom_q16), 16);
    for n in 0..frame_length {
        if n < interp {
            pred0 += delta0;
            pred1 += delta1;
        } else {
            pred0 = pred_q13[0];
            pred1 = pred_q13[1];
        }
        let sum = ((x1[n] as i32 + x1[n + 2] as i32) + ((x1[n + 1] as i32) << 1)) << 9;
        let mut acc = smlawb((x2[n + 1] as i32) << 8, sum, pred0);
        acc = smlawb(acc, (x1[n + 1] as i32) << 11, pred1);
        x2[n + 1] = sat16(rshift_round(acc, 8));
    }
    state.pred_prev_q13[0] = pred_q13[0];
    state.pred_prev_q13[1] = pred_q13[1];

    for n in 0..frame_length {
        let sum = x1[n + 1] as i32 + x2[n + 1] as i32;
        let diff = x1[n + 1] as i32 - x2[n + 1] as i32;
        x1[n + 1] = sat16(sum);
        x2[n + 1] = sat16(diff);
    }
}

// ---------------------------------------------------------------------------
// Resampling (Section 4.2.9)
// ---------------------------------------------------------------------------

/// The SILK output resampler: 8/12/16 kHz internal to the API rate.
///
/// The RFC calls the resampler non-normative but its *delay* normative, so this
/// is the reference's structure — a 2x all-pass upsampler followed by a
/// 12-phase fractional FIR, with the same per-rate input delay.
#[derive(Clone, Debug, Default)]
struct Resampler {
    fs_in_khz: usize,
    fs_out_khz: usize,
    input_delay: usize,
    batch_size: usize,
    inv_ratio_q16: i32,
    /// All-pass state for the 2x upsampler.
    s_iir: [i32; 6],
    /// FIR history.
    s_fir: [i16; 8],
    delay_buf: DelayBuf,
    /// Upsampler scratch, allocated once.
    fir_buf: Vec<i16>,
    /// Down-sampling coefficients, when the output rate is the lower one.
    down: Option<DownFir>,
}

/// Fixed-size delay line; a newtype only because `[i16; 48]` has no `Default`.
#[derive(Clone, Copy, Debug)]
struct DelayBuf([i16; 48]);

impl Default for DelayBuf {
    fn default() -> Self {
        DelayBuf([0; 48])
    }
}

impl core::ops::Deref for DelayBuf {
    type Target = [i16; 48];
    fn deref(&self) -> &[i16; 48] {
        &self.0
    }
}

impl core::ops::DerefMut for DelayBuf {
    fn deref_mut(&mut self) -> &mut [i16; 48] {
        &mut self.0
    }
}

#[derive(Clone, Debug)]
struct DownFir {
    fracs: usize,
    order: usize,
    coefs: &'static [i16],
    state: [i32; 2],
    buf: Vec<i32>,
}

/// `rateID` in the reference: an index into the delay matrix.
fn rate_id(rate: usize) -> usize {
    (((rate >> 12) - usize::from(rate > 16000)) >> usize::from(rate > 24000)) - 1
}

const DELAY_MATRIX_DEC: [[u8; 5]; 3] = [[4, 0, 2, 0, 0], [0, 9, 4, 7, 4], [0, 3, 12, 7, 7]];

impl Resampler {
    fn new(fs_in_khz: usize, fs_out_khz: usize) -> Resampler {
        let fs_in = fs_in_khz * 1000;
        let fs_out = fs_out_khz * 1000;
        let mut r = Resampler {
            fs_in_khz,
            fs_out_khz,
            input_delay: DELAY_MATRIX_DEC[rate_id(fs_in)][rate_id(fs_out)] as usize,
            batch_size: fs_in_khz * 10,
            ..Default::default()
        };
        let mut up2x = 0;
        if fs_out > fs_in && fs_out != 2 * fs_in {
            up2x = 1;
        } else if fs_out < fs_in {
            let (fracs, order, coefs): (usize, usize, &'static [i16]) = if fs_out * 4 == fs_in * 3 {
                (3, 18, &RESAMPLER_3_4_COEFS)
            } else if fs_out * 3 == fs_in * 2 {
                (2, 18, &RESAMPLER_2_3_COEFS)
            } else if fs_out * 2 == fs_in {
                (1, 24, &RESAMPLER_1_2_COEFS)
            } else if fs_out * 3 == fs_in {
                (1, 36, &RESAMPLER_1_3_COEFS)
            } else if fs_out * 4 == fs_in {
                (1, 36, &RESAMPLER_1_4_COEFS)
            } else {
                (1, 36, &RESAMPLER_1_6_COEFS)
            };
            r.down = Some(DownFir {
                fracs,
                order,
                coefs,
                state: [0; 2],
                buf: vec![0; 480 + 36],
            });
        }
        r.inv_ratio_q16 = ((((fs_in as i64) << (14 + up2x)) / fs_out as i64) as i32) << 2;
        while smulww(r.inv_ratio_q16, fs_out as i32) < ((fs_in as i32) << up2x) {
            r.inv_ratio_q16 += 1;
        }
        r
    }

    /// Resamples `in_len` input samples into `in_len * out/in` outputs.
    ///
    /// The first `Fs_in_kHz` samples come from the delay line (the normative
    /// per-rate input delay); the rest are read straight from `input`, and its
    /// tail is kept for the next call.
    fn process(&mut self, out: &mut [i16], input: &[i16], in_len: usize) {
        let n_first = self.fs_in_khz - self.input_delay;
        let mut head = [0i16; 48];
        head[..self.input_delay].copy_from_slice(&self.delay_buf[..self.input_delay]);
        head[self.input_delay..self.fs_in_khz].copy_from_slice(&input[..n_first]);
        let n_head = self.fs_in_khz;
        let rest = &input[n_first..n_first + (in_len - self.fs_in_khz)];
        let split = self.fs_out_khz;
        let rest_out = (in_len - self.fs_in_khz) * self.fs_out_khz / self.fs_in_khz;
        if self.fs_in_khz == self.fs_out_khz {
            out[..n_head].copy_from_slice(&head[..n_head]);
            out[split..split + rest_out].copy_from_slice(rest);
        } else if self.down.is_some() {
            self.down_fir(&mut out[..split], &head[..n_head]);
            self.down_fir(&mut out[split..split + rest_out], rest);
        } else if self.fs_out_khz == 2 * self.fs_in_khz {
            self.up2_hq(&mut out[..split], &head[..n_head]);
            self.up2_hq(&mut out[split..split + rest_out], rest);
        } else {
            self.iir_fir(&mut out[..split], &head[..n_head]);
            self.iir_fir(&mut out[split..split + rest_out], rest);
        }
        self.delay_buf[..self.input_delay]
            .copy_from_slice(&input[in_len - self.input_delay..in_len]);
    }

    /// `silk_resampler_private_up2_HQ`: 2x all-pass upsampling.
    fn up2_hq(&mut self, out: &mut [i16], input: &[i16]) {
        for (k, &sample) in input.iter().enumerate() {
            let in32 = (sample as i32) << 10;
            let mut y = in32 - self.s_iir[0];
            let mut x = smulwb(y, RESAMPLER_UP2_HQ_0[0] as i32);
            let mut out32_1 = self.s_iir[0] + x;
            self.s_iir[0] = in32 + x;
            y = out32_1 - self.s_iir[1];
            x = smulwb(y, RESAMPLER_UP2_HQ_0[1] as i32);
            let mut out32_2 = self.s_iir[1] + x;
            self.s_iir[1] = out32_1 + x;
            y = out32_2 - self.s_iir[2];
            x = smlawb(y, y, RESAMPLER_UP2_HQ_0[2] as i32);
            out32_1 = self.s_iir[2] + x;
            self.s_iir[2] = out32_2 + x;
            out[2 * k] = sat16(rshift_round(out32_1, 10));

            y = in32 - self.s_iir[3];
            x = smulwb(y, RESAMPLER_UP2_HQ_1[0] as i32);
            out32_1 = self.s_iir[3] + x;
            self.s_iir[3] = in32 + x;
            y = out32_1 - self.s_iir[4];
            x = smulwb(y, RESAMPLER_UP2_HQ_1[1] as i32);
            out32_2 = self.s_iir[4] + x;
            self.s_iir[4] = out32_1 + x;
            y = out32_2 - self.s_iir[5];
            x = smlawb(y, y, RESAMPLER_UP2_HQ_1[2] as i32);
            out32_1 = self.s_iir[5] + x;
            self.s_iir[5] = out32_2 + x;
            out[2 * k + 1] = sat16(rshift_round(out32_1, 10));
        }
    }

    /// `silk_resampler_private_IIR_FIR`: 2x upsample then fractional FIR.
    fn iir_fir(&mut self, out: &mut [i16], input: &[i16]) {
        let mut written = 0usize;
        let mut pos = 0usize;
        let mut buf = core::mem::take(&mut self.fir_buf);
        buf.clear();
        buf.resize(2 * self.batch_size + 8, 0);
        buf[..8].copy_from_slice(&self.s_fir);
        let mut n_samples_in = 0;
        while pos < input.len() {
            n_samples_in = (input.len() - pos).min(self.batch_size);
            self.up2_hq(
                &mut buf[8..8 + 2 * n_samples_in],
                &input[pos..pos + n_samples_in],
            );
            let max_index_q16 = (n_samples_in as i32) << 17;
            let mut index_q16 = 0i32;
            while index_q16 < max_index_q16 {
                let table_index = smulwb(index_q16 & 0xFFFF, 12) as usize;
                let b = &buf[(index_q16 >> 16) as usize..];
                let mut res = smulbb(b[0] as i32, RESAMPLER_FRAC_FIR_12[table_index][0] as i32);
                res = smlabb(
                    res,
                    b[1] as i32,
                    RESAMPLER_FRAC_FIR_12[table_index][1] as i32,
                );
                res = smlabb(
                    res,
                    b[2] as i32,
                    RESAMPLER_FRAC_FIR_12[table_index][2] as i32,
                );
                res = smlabb(
                    res,
                    b[3] as i32,
                    RESAMPLER_FRAC_FIR_12[table_index][3] as i32,
                );
                res = smlabb(
                    res,
                    b[4] as i32,
                    RESAMPLER_FRAC_FIR_12[11 - table_index][3] as i32,
                );
                res = smlabb(
                    res,
                    b[5] as i32,
                    RESAMPLER_FRAC_FIR_12[11 - table_index][2] as i32,
                );
                res = smlabb(
                    res,
                    b[6] as i32,
                    RESAMPLER_FRAC_FIR_12[11 - table_index][1] as i32,
                );
                res = smlabb(
                    res,
                    b[7] as i32,
                    RESAMPLER_FRAC_FIR_12[11 - table_index][0] as i32,
                );
                if written < out.len() {
                    out[written] = sat16(rshift_round(res, 15));
                }
                written += 1;
                index_q16 += self.inv_ratio_q16;
            }
            pos += n_samples_in;
            if pos < input.len() {
                buf.copy_within(2 * n_samples_in..2 * n_samples_in + 8, 0);
            }
        }
        self.s_fir
            .copy_from_slice(&buf[2 * n_samples_in..2 * n_samples_in + 8]);
        self.fir_buf = buf;
    }

    /// `silk_resampler_private_down_FIR`, for output rates below the internal
    /// rate (an AR2 pre-filter, then a polyphase FIR).
    fn down_fir(&mut self, out: &mut [i16], input: &[i16]) {
        let inv_ratio = self.inv_ratio_q16;
        let d = self.down.as_mut().expect("down-sampling state");
        let order = d.order;
        let mut buf = core::mem::take(&mut d.buf);
        // AR2 pre-filter into Q8.
        let a_q14 = [d.coefs[0], d.coefs[1]];
        for (k, &s) in input.iter().enumerate() {
            let out32 = d.state[0] + ((s as i32) << 8);
            buf[order + k] = out32;
            let out32 = out32 << 2;
            d.state[0] = smlawb(d.state[1], out32, a_q14[0] as i32);
            d.state[1] = smulwb(out32, a_q14[1] as i32);
        }
        let fir = &d.coefs[2..];
        let fracs = d.fracs;
        let mut written = 0usize;
        let max_index_q16 = (input.len() as i32) << 16;
        let mut index_q16 = 0i32;
        while index_q16 < max_index_q16 && written < out.len() {
            let base = (index_q16 >> 16) as usize;
            let interpol_ind = smulwb(index_q16 & 0xFFFF, fracs as i32) as usize;
            let coefs = &fir[interpol_ind * (order / 2)..];
            let mut res = 0i64;
            for j in 0..order / 2 {
                res += buf[base + j] as i64 * coefs[j] as i64;
                res += buf[base + order - 1 - j] as i64 * coefs[j] as i64;
            }
            out[written] = sat16(rshift_round((res >> 16) as i32, 6));
            written += 1;
            index_q16 += inv_ratio;
        }
        let keep = order;
        buf.copy_within(input.len()..input.len() + keep, 0);
        d.buf = buf;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_point_helpers_match_their_definitions() {
        // Spot values computed from the reference macro definitions.
        assert_eq!(smulwb(1 << 20, 3), 48);
        assert_eq!(smulbb(-5, 7), -35);
        assert_eq!(rshift_round(7, 1), 4);
        assert_eq!(rshift_round(-7, 2), -2);
        assert_eq!(sat16(40000), 32767);
        assert_eq!(sat16(-40000), -32768);
        // log2lin is the exponential the gain quantiser uses: 2^(q/128).
        for q in [0i32, 128, 1000, 3967] {
            let got = log2lin(q) as f64;
            let want = (q as f64 / 128.0).exp2();
            assert!(
                (got - want).abs() / want < 0.02,
                "log2lin({q}) = {got}, want {want}"
            );
        }
        // sqrt_approx returns sqrt(x) in the same Q domain. It is a coarse
        // approximation whose relative error only settles below ~1 % once the
        // input has enough significant bits, which is where SILK uses it (the
        // NLSF weights are shifted up by 16 first).
        for x in [1 << 18, 1234567, 1 << 24, 1 << 30] {
            let y = sqrt_approx(x) as f64;
            let want = (x as f64).sqrt();
            assert!(
                (y - want).abs() / want < 0.02,
                "sqrt({x}) = {y}, want {want}"
            );
        }
    }

    #[test]
    fn gains_are_monotonic_in_the_index() {
        let mut prev = 0;
        let mut last = 0;
        for i in 0..64i8 {
            let g = gains_dequant(&[i, 0, 0, 0], &mut prev, false, 1)[0];
            assert!(g > last, "gain for index {i} did not increase");
            last = g;
            prev = i as i32;
        }
    }

    #[test]
    fn nlsf_stabilize_enforces_the_minimum_spacing() {
        let cb = nlsf_cb(false);
        let mut nlsf = [100i16, 120, 130, 140, 150, 160, 170, 180, 190, 32700];
        nlsf_stabilize(&mut nlsf, cb.delta_min_q15);
        for i in 1..10 {
            assert!(
                nlsf[i] - nlsf[i - 1] >= cb.delta_min_q15[i],
                "spacing at {i}: {} < {}",
                nlsf[i] - nlsf[i - 1],
                cb.delta_min_q15[i]
            );
        }
        assert!(nlsf[0] >= cb.delta_min_q15[0]);
        assert!((1 << 15) - nlsf[9] as i32 >= cb.delta_min_q15[10] as i32);
    }
}

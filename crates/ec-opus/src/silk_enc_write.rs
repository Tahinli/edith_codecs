//! SILK payload writer (subtask D2a): turns the analysis in [`crate::silk_enc`]
//! into decodable SILK packets, NB (8 kHz), MB (12 kHz) or WB (16 kHz).
//!
//! Symbol order is exactly `silk::decode()`'s, and every decision is made
//! against the decoder's own tables and reconstruction functions. The
//! encoder keeps a [`SilkDecoder`] of its own and feeds it every packet it
//! writes, so the synthesis history its LTP search and residual computation
//! use *is* the real decoder's state — there is no encoder-side model of the
//! decoder that could drift.
//!
//! The excitation is coded by a noise-shaping quantiser: the decoder's
//! output error is fed back through short-term (bandwidth-expanded LPC),
//! harmonic and tilt shaping filters so the noise hides under the signal's
//! own spectrum, and a gain-offset rate loop sizes each packet against
//! [`SilkEncoder::set_bitrate`].

// Same idiom as silk.rs: index loops mirror the decoder's position arithmetic.
#![allow(clippy::needless_range_loop)]

use ec_core::Result;

use crate::range::{RangeDecoder, RangeEncoder};
use crate::silk::tables::*;
use crate::silk::{
    MAX_LPC_ORDER, MAX_NB_SUBFR, PE_MAX_LAG_MS, PE_MIN_LAG_MS, SilkDecoder, decode_pitch, nlsf_cb,
    nlsf2a, silk_rand,
};
use crate::silk_enc::{
    NlsfQuant, Resampler48, Vad, estimate_pitch, lpc_analyze, quantize_gains, quantize_nlsf,
};

const SHELL_LEN: usize = 16;
const LTP_ORDER: usize = 5;
/// `QUANT_LEVEL_ADJUST_Q10` in `decode_core`: a non-zero pulse decodes 80/1024
/// closer to zero than its integer value.
const LEVEL_ADJUST: f32 = 80.0 / 1024.0;
/// Largest pulse magnitude the shell coder's 10-level LSB escape can carry
/// with 16 such pulses in one block.
const MAX_PULSE: i32 = 1023;

/// Per-frame SILK diagnostics captured at the end of the last `encode_inner`
/// call, for the speech-quality diagnostic lane.
#[derive(Clone, Debug, Default)]
pub struct SilkFrameDiag {
    pub voiced: bool,
    pub signal_type: i32,
    pub gain_idx: [i8; MAX_NB_SUBFR],
    pub nb_subfr: usize,
    pub lag_index: i32,
    pub pitch_l: [i32; MAX_NB_SUBFR],
    pub nlsf_interp: i32,
    pub bytes: usize,
    pub ltp_gain: f32,
}

/// A mono, 10/20/40/60 ms, NB/MB/WB SILK encoder.
#[derive(Clone, Debug)]
pub struct SilkEncoder {
    fs_khz: usize,
    resampler: Resampler48,
    vad: Vad,
    /// Replays every packet this encoder writes: its state is the decoder's.
    mirror: SilkDecoder,
    range: RangeEncoder,
    prev_gain_index: i32,
    final_range: u32,
    /// The mirror decoder's range state after the last replay.
    mirror_range: u32,
    voiced_frames: usize,
    mirror_out: Vec<i16>,
    target_bps: Option<u32>,
    /// Bits the rate loop owes (+) or banked (-) against the target.
    reservoir: f32,
    /// Last frame's gain-step operating point (libopus carries its gain
    /// multiplier's effect through `LastGainIndex`; the x4 bound is per frame).
    rate_step: i32,
    rate_ema: f32,
    /// Noise-shaping state: output error, its AR-filtered form, and the
    /// shaped quantiser error, for the last frame.
    shape_d: Vec<f32>,
    shape_u: Vec<f32>,
    shape_gq: Vec<f32>,
    prev_pitch_lag: i32,
    last_diag: SilkFrameDiag,
}

/// Noise-shaping constants: AR (denominator) and MA (numerator) bandwidth
/// expansions of the LPC, harmonic comb depth, voiced spectral tilt, and the
/// linear ratio of one gain-quantiser step (2^(1.365 dB / 6.02)).
const SHAPE_AR: f32 = 0.96;
const SHAPE_MA: f32 = 0.85;
const SHAPE_HARMONIC: f32 = 0.3;
const SHAPE_TILT: f32 = 0.1;
const GAIN_STEP: f32 = 1.1702;
/// Frames' worth of bits the rate loop may bank or borrow.
const RESERVOIR_FRAMES: f32 = 4.0;
/// libopus reservoir semantics for the SILK rate loop (see the loop).
const LIBOPUS_RESERVOIR: bool = true;
/// libopus `BITRESERVOIR_DECAY_TIME_MS`.
const BITRESERVOIR_DECAY_TIME_MS: f32 = 500.0;
/// libopus clamps `nBitsExceeded` to 0..10000.
const MAX_BITS_EXCEEDED: f32 = 10000.0;
/// Quantiser steps in libopus's x4 gain-multiplier bound (1024/256 in Q8).
const MAX_GAIN_STEPS: i32 = 9;
/// libopus `MIN_TARGET_RATE_BPS`.
const MIN_TARGET_RATE_BPS: f32 = 5000.0;
/// Upper bound on the carried operating point (a channel whose share is
/// below the rate floor otherwise walks the gain up 9 steps a frame
/// forever). Below, libopus's absolute x0.25 bound holds: a near-silent
/// stereo side channel spending up to its share walked the gain down 30
/// steps and drowned the mid (stereo 16k oracle corr .07).
const MAX_RATE_STEP: i32 = 30;
/// Start each frame's rate loop at the previous frame's operating point.
const CARRY_RATE_STEP: bool = true;
/// Operating-point smoothing: the loop starts each frame at the slow
/// average of past steps (libopus derives its gains from the target rate
/// before its x4-bounded loop; carrying the previous frame's exact point
/// instead cost sadie@12k .05 corr).
const CARRY_EMA: f32 = 1.0 / 16.0;
const TARGET_FLOOR: bool = true;
/// Rate penalty per unit pulse magnitude in the quantiser's squared-error
/// cost: a dead zone that lets the rate loop keep finer gains for the same
/// bits. NB 20 ms speech wants a smaller dead zone; 10 ms WB needs a
/// slightly smaller one than the 10 ms NB/MB rows to keep upper-band pulses.
const PULSE_LAMBDA: f32 = 1.5;

impl SilkEncoder {
    /// `wideband`: 16 kHz internal rate (TOC config 9), else 8 kHz (config 1).
    pub fn new(wideband: bool) -> SilkEncoder {
        Self::with_fs_khz(if wideband { 16 } else { 8 })
    }

    /// Mediumband, 12 kHz internal rate (TOC config 5).
    pub fn new_mediumband() -> SilkEncoder {
        Self::with_fs_khz(12)
    }

    fn with_fs_khz(fs_khz: usize) -> SilkEncoder {
        debug_assert!(matches!(fs_khz, 8 | 12 | 16));
        SilkEncoder {
            fs_khz,
            resampler: Resampler48::new(fs_khz as u32),
            vad: Vad::new(),
            mirror: SilkDecoder::new(fs_khz as u32 * 1000, 1),
            range: RangeEncoder::new(),
            prev_gain_index: 10,
            final_range: 0,
            mirror_range: 0,
            voiced_frames: 0,
            prev_pitch_lag: 0,
            mirror_out: vec![0; 20 * 16],
            target_bps: None,
            reservoir: 0.0,
            rate_step: 0,
            rate_ema: 0.0,
            shape_d: vec![0.0; 20 * fs_khz],
            shape_u: vec![0.0; 20 * fs_khz],
            shape_gq: vec![0.0; 20 * fs_khz],
            last_diag: SilkFrameDiag::default(),
        }
    }

    /// The range coder state after the last packet; a conformant decoder
    /// reports the same value after decoding it.
    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    /// Frames so far coded with the long-term (pitch) predictor.
    pub fn voiced_frames(&self) -> usize {
        self.voiced_frames
    }

    /// Per-frame diagnostics captured at the end of the last `encode_inner`
    /// call.
    pub fn last_diag(&self) -> &SilkFrameDiag {
        &self.last_diag
    }

    /// Target bit rate in bits per second; `None` (the default) codes pulses
    /// at unit excitation RMS with no rate loop.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.target_bps = Some(bps);
    }

    fn reset_state(&mut self) {
        *self = Self::with_fs_khz(self.fs_khz);
    }

    /// Algorithmic delay in 48 kHz samples: the analysis resampler only
    /// (the decoder adds its own resampler delay on the way back up).
    pub fn delay_samples(&self) -> usize {
        self.resampler.delay_samples()
    }

    /// Encodes 960 mono 48 kHz samples as one 20 ms code-0 packet in `out`,
    /// returning its length.
    pub fn encode_frame(&mut self, pcm48: &[f32], out: &mut [u8]) -> Result<usize> {
        self.encode_frame_ms(pcm48, out, 20)
    }

    /// Encodes 10, 20, 40 or 60 ms of mono 48 kHz samples as one code-0
    /// packet in `out`, returning its length.
    pub fn encode_frame_ms(
        &mut self,
        pcm48: &[f32],
        out: &mut [u8],
        frame_ms: usize,
    ) -> Result<usize> {
        let frames_per_packet = match frame_ms {
            10 | 20 => 1usize,
            40 => 2,
            60 => 3,
            _ => {
                return Err(ec_core::Error::unsupported(
                    format!("silk encode frame of {frame_ms} ms"),
                    "SILK encoder writes 10, 20, 40 or 60 ms packets",
                ));
            }
        };
        assert_eq!(
            pcm48.len(),
            48 * frame_ms,
            "SilkEncoder input is 48 kHz mono"
        );
        let mut frames = Vec::with_capacity(frames_per_packet);
        let sub_ms = if frame_ms == 10 { 10 } else { 20 };
        let mut prev_signal_type = 0;
        for i in 0..frames_per_packet {
            let from = i * sub_ms * 48;
            let to = from + sub_ms * 48;
            let conditional = i > 0;
            let trial = self.encode_inner(
                &pcm48[from..to],
                sub_ms,
                None,
                conditional,
                prev_signal_type,
                frames_per_packet == 1,
            )?;
            prev_signal_type = trial.signal_type;
            frames.push(trial);
        }
        if frames.iter().any(|frame| frame.voiced) {
            self.voiced_frames += 1;
        }
        self.range.reset(1275);
        let refs: Vec<&Trial> = frames.iter().collect();
        write_packet_header(&mut self.range, &refs);
        prev_signal_type = 0;
        for (i, frame) in frames.iter().enumerate() {
            write_frame_data(&mut self.range, frame, i > 0, prev_signal_type);
            prev_signal_type = frame.signal_type;
        }
        self.range.done();
        debug_assert!(!self.range.error());
        let len = self.range.range_bytes();
        self.final_range = self.range.range();
        let duration = match frame_ms {
            10 => 0u8,
            20 => 1,
            40 => 2,
            60 => 3,
            _ => unreachable!(),
        };
        let base = match self.fs_khz {
            8 => 0u8,
            12 => 4,
            _ => 8,
        };
        let toc = (base + duration) << 3;
        if out.len() < 1 + len {
            return Err(ec_core::Error::corrupt(format!(
                "silk encode: packet needs {} bytes, buffer holds {}",
                1 + len,
                out.len()
            )));
        }
        out[0] = toc;
        out[1..1 + len].copy_from_slice(&self.range.data()[..len]);
        self.replay_ms(&out[1..1 + len], frame_ms)?;
        debug_assert_eq!(self.mirror_range, self.final_range);
        Ok(1 + len)
    }

    /// The SILK layer of a hybrid packet: codes one mono 48 kHz frame into
    /// `enc`, the packet's shared range coder (SILK symbols first; the CELT
    /// layer continues in the same coder), without finishing it. The caller
    /// must [`SilkEncoder::replay_hybrid`] the finished payload so this
    /// encoder's decoder mirror advances.
    pub fn encode_hybrid_ms(
        &mut self,
        pcm48: &[f32],
        enc: &mut RangeEncoder,
        frame_ms: usize,
    ) -> Result<()> {
        let trial = self.encode_inner(pcm48, frame_ms, Some(enc), false, 0, true)?;
        self.voiced_frames += usize::from(trial.voiced);
        Ok(())
    }

    /// Decodes a finished hybrid payload (TOC stripped) through the mirror
    /// decoder so the next frame predicts from the decoder's state.
    pub fn replay_hybrid(&mut self, payload: &[u8], frame_ms: usize) -> Result<()> {
        self.replay_ms(payload, frame_ms)
    }

    fn replay_ms(&mut self, payload: &[u8], frame_ms: usize) -> Result<()> {
        let fs = self.fs_khz;
        let mut dec = RangeDecoder::new(payload);
        let total = frame_ms * fs;
        if self.mirror_out.len() < total {
            self.mirror_out.resize(total, 0);
        }
        let mut done = 0usize;
        let mut first = true;
        while done < total {
            let n = self.mirror.decode(
                &mut dec,
                &mut self.mirror_out[done..],
                frame_ms,
                fs as u32 * 1000,
                1,
                first,
            )?;
            if n == 0 {
                break;
            }
            done += n;
            first = false;
        }
        self.mirror_range = dec.range();
        Ok(())
    }

    /// Analysis, closed-loop quantisation and rate control of one frame; the
    /// returned trial's `bytes` are the standalone SILK payload (TOC
    /// excluded). With `shared`, the chosen frame's symbols are also written
    /// into that coder (the hybrid path).
    fn encode_inner(
        &mut self,
        pcm48: &[f32],
        frame_ms: usize,
        shared: Option<&mut RangeEncoder>,
        conditional: bool,
        prev_signal_type: i32,
        single_frame_packet: bool,
    ) -> Result<Trial> {
        assert!(
            matches!(frame_ms, 10 | 20),
            "SilkEncoder codes 10 or 20 ms mono frames"
        );
        assert_eq!(
            pcm48.len(),
            48 * frame_ms,
            "SilkEncoder input is 48 kHz mono"
        );
        let fs = self.fs_khz;
        let order = if fs == 16 { MAX_LPC_ORDER } else { 10 };
        let nb_subfr = frame_ms / 5;
        let n = frame_ms * fs;
        let sub = 5 * fs;
        if self.shape_d.len() != n {
            self.shape_d.resize(n, 0.0);
            self.shape_u.resize(n, 0.0);
            self.shape_gq.resize(n, 0.0);
        }
        let x: Vec<f32> = self
            .resampler
            .process(pcm48)
            .iter()
            .map(|v| v * 32768.0)
            .collect();
        let vad = self.vad.analyze(&x);

        // LPC -> NLSF indices -> the decoder's dequantised filter.
        let nlsf_target = lpc_analyze(&x, order)
            .map(|a| a.nlsf_q15)
            .unwrap_or_else(|| {
                let mut flat = [0i16; MAX_LPC_ORDER];
                for (i, v) in flat.iter_mut().enumerate().take(order) {
                    *v = ((i + 1) * 32768 / (order + 1)) as i16;
                }
                flat
            });
        let wb = fs == 16;
        let nq = quantize_nlsf(&nlsf_target[..order], wb);
        let a_q12 = nlsf2a(&nq.nlsf_q15[..order], order);
        let a: Vec<f32> = a_q12[..order].iter().map(|&v| v as f32 / 4096.0).collect();

        // Residual of this frame with the decoder's real past output as filter
        // memory, and the whitened past output itself (the LTP history the
        // decoder rebuilds at subframe 0).
        // (Empty before the first packet: the decoder's history is all zero.)
        let mut hist: Vec<f32> = self.mirror.history().iter().map(|&v| v as f32).collect();
        hist.resize(n, 0.0);
        let sample = |i: isize| -> f32 {
            if i < 0 {
                hist[(n as isize + i) as usize]
            } else {
                x[i as usize]
            }
        };
        let whiten = |i: isize| -> f32 {
            let mut acc = sample(i);
            for (j, &aj) in a.iter().enumerate() {
                let idx = i - 1 - j as isize;
                if idx >= -(n as isize) {
                    acc -= aj * sample(idx);
                }
            }
            acc
        };
        let res: Vec<f32> = (0..n as isize).map(whiten).collect();

        // Pitch / LTP.
        let prior = if self.prev_pitch_lag > 0 { Some(self.prev_pitch_lag) } else { None };
        let pitch = estimate_pitch(&x, fs as i32, nb_subfr, prior).filter(|p| p.voiced);
        if let Some(p) = &pitch {
            self.prev_pitch_lag = p.lag;
        } else {
            self.prev_pitch_lag = 0;
        }
        let voiced = pitch.is_some();

        let (lag_index, contour_index, pitch_l) = match &pitch {
            Some(p) => {
                let max_index = (PE_MAX_LAG_MS - PE_MIN_LAG_MS) * fs as i32 - 1;
                let li = p.lag_index.clamp(0, max_index);
                (
                    li,
                    p.contour_index,
                    decode_pitch(li, p.contour_index, fs as i32, nb_subfr),
                )
            }
            None => (0, 0, [0; MAX_NB_SUBFR]),
        };
        let ltp_pred = |r: &[f32], b: &[i8; 5], k: usize, i: usize| -> f32 {
            let pos = n + k * sub + i - pitch_l[k] as usize + LTP_ORDER / 2;
            let mut acc = 0f32;
            for (j, &bj) in b.iter().enumerate() {
                acc += bj as f32 / 128.0 * r[pos - j];
            }
            acc
        };
        let codebook = |per: usize| -> &'static [[i8; 5]] {
            match per {
                0 => &LTP_GAIN_VQ_0,
                1 => &LTP_GAIN_VQ_1,
                _ => &LTP_GAIN_VQ_2,
            }
        };

        // Open-loop pass: the excitation RMS per subframe (best LTP vector
        // against the unquantised residual) sets the gain targets; the tap sum
        // of the best vector is the LTP gain that picks the scale index.
        let mut targets = [0i32; MAX_NB_SUBFR];
        let mut ltp_gain = 0f32;
        {
            let mut r_ol: Vec<f32> = (-(n as isize)..0).map(whiten).collect();
            r_ol.extend_from_slice(&res);
            for k in 0..nb_subfr {
                let (e, gsum) = if voiced {
                    LTP_GAIN_VQ_2
                        .iter()
                        .map(|b| {
                            let e = (0..sub)
                                .map(|i| (res[k * sub + i] - ltp_pred(&r_ol, b, k, i)).powi(2))
                                .sum::<f32>();
                            (e, b.iter().map(|&v| v as f32 / 128.0).sum::<f32>())
                        })
                        .fold((f32::INFINITY, 0.0), |a, b| if b.0 < a.0 { b } else { a })
                } else {
                    ((0..sub).map(|i| res[k * sub + i].powi(2)).sum(), 0.0)
                };
                ltp_gain = ltp_gain.max(gsum);
                let rms = (e / sub as f32).sqrt();
                targets[k] = ((rms * 65536.0).round() as i64).clamp(1, i32::MAX as i64) as i32;
            }
        }
        let vad_flag = vad.active || voiced;
        let signal_type: i32 = if !vad_flag {
            0
        } else if voiced {
            2
        } else {
            1
        };
        // Over-unity LTP gain: scale the previous frame's history down so
        // quantisation error does not build up through the pitch loop.
        let ltp_scale_index = if !voiced {
            0
        } else if ltp_gain > 1.25 {
            2
        } else if ltp_gain > 1.0 {
            1
        } else {
            0
        };
        let ltp_scale = LTPSCALES_TABLE_Q14[ltp_scale_index] as f32 / 16384.0;
        // Inactive or low-energy frames (and sparse voiced excitation) take the
        // small quantisation offset; dense unvoiced excitation the large one.
        let mean_rms = targets[..nb_subfr]
            .iter()
            .map(|&t| t as f32 / 65536.0)
            .sum::<f32>()
            / nb_subfr as f32;
        let quant_offset_type = usize::from(signal_type == 1 && mean_rms > 64.0);
        let offset =
            QUANTIZATION_OFFSETS_Q10[signal_type as usize >> 1][quant_offset_type] as f32 / 1024.0;

        // Noise shaping filters, from the bandwidth-expanded LPC: the output
        // error d follows A(z/g2)(1 - tilt z^-1) / (A(z/g1)(1 - h z^-L)), i.e.
        // the quantisation noise sits under the formants and, when voiced,
        // under the harmonics, instead of being white at the decoder output.
        let (g1, g2, tilt) = (SHAPE_AR, SHAPE_MA, if voiced { SHAPE_TILT } else { 0.0 });
        let c: Vec<f32> = a
            .iter()
            .enumerate()
            .map(|(j, &aj)| aj * g1.powi(j as i32 + 1))
            .collect();
        let b2: Vec<f32> = a
            .iter()
            .enumerate()
            .map(|(j, &aj)| aj * g2.powi(j as i32 + 1))
            .collect();
        // (1 - B2(z))(1 - tilt z^-1) = 1 - NB(z).
        let mut nb = vec![0f32; order + 1];
        for j in 0..order {
            nb[j] += b2[j];
            nb[j + 1] -= tilt * b2[j];
        }
        nb[0] += tilt;
        let harm = if voiced {
            SHAPE_HARMONIC * ltp_gain.clamp(0.0, 1.0)
        } else {
            0.0
        };

        // `r`: LTP history in absolute sample units, past frame first (scaled
        // by ltp_scale as the decoder scales its rebuilt history), then this
        // frame's reconstructed residual as it is quantised.
        let mut r: Vec<f32> = (-(n as isize)..0).map(|i| whiten(i) * ltp_scale).collect();
        r.resize(2 * n, 0.0);
        let seed_index = 0i32;

        // Noise-shaping quantiser, closed loop: per subframe pick the LTP
        // vector that best predicts the residual from the *reconstructed*
        // history, then quantise sample by sample with the shaped output error
        // fed back into the target, under the decoder's seed-driven sign flips.
        let pulse_lambda = match (single_frame_packet, frame_ms, fs) {
            (true, 20, 8) => 1.25,
            (_, 10, 16) => 1.45,
            _ => PULSE_LAMBDA,
        };
        let run = |per: usize, gains_q16: &[i32; MAX_NB_SUBFR]| -> Run {
            let cbk = codebook(per);
            let mut r = r.clone();
            let mut y = hist.clone();
            y.resize(2 * n, 0.0);
            let mut d = self.shape_d.clone();
            d.resize(2 * n, 0.0);
            let mut u = self.shape_u.clone();
            u.resize(2 * n, 0.0);
            let mut gq = self.shape_gq.clone();
            gq.resize(2 * n, 0.0);
            let mut ltp_index = [0usize; MAX_NB_SUBFR];
            let mut pulses = vec![0i32; n];
            let mut seed = seed_index;
            let mut err = 0f32;
            for k in 0..nb_subfr {
                if voiced {
                    // In-subframe taps (lag shorter than the subframe) see the
                    // unquantised residual as a stand-in.
                    let mut r_ol = r.clone();
                    r_ol[n + k * sub..n + (k + 1) * sub]
                        .copy_from_slice(&res[k * sub..(k + 1) * sub]);
                    let mut best = f32::INFINITY;
                    for (v, b) in cbk.iter().enumerate() {
                        let e: f32 = (0..sub)
                            .map(|i| (res[k * sub + i] - ltp_pred(&r_ol, b, k, i)).powi(2))
                            .sum();
                        if e < best {
                            best = e;
                            ltp_index[k] = v;
                        }
                    }
                }
                let g = gains_q16[k] as f32 / 65536.0;
                let lag = pitch_l[k] as usize;
                for i in 0..sub {
                    let idx = k * sub + i;
                    let p = n + idx;
                    let lpc: f32 = a.iter().enumerate().map(|(j, &aj)| aj * y[p - 1 - j]).sum();
                    let pred = if voiced {
                        ltp_pred(&r, &cbk[ltp_index[k]], k, i)
                    } else {
                        0.0
                    };
                    let mut s: f32 = c.iter().enumerate().map(|(j, &cj)| cj * d[p - 1 - j]).sum();
                    s -= nb
                        .iter()
                        .enumerate()
                        .map(|(j, &bj)| bj * gq[p - 1 - j])
                        .sum::<f32>();
                    if harm > 0.0 {
                        s += harm * u[p - lag];
                    }
                    let e = (x[idx] - lpc - pred + s) / g;
                    seed = silk_rand(seed);
                    let flip = seed < 0;
                    let target = if flip { -e } else { e };
                    let recon = |p: i32| -> f32 {
                        let lvl = p as f32 - LEVEL_ADJUST * (p.signum() as f32);
                        lvl + offset
                    };
                    let c0 = (target.round() as i32).clamp(-MAX_PULSE, MAX_PULSE);
                    let pl = [c0 - 1, c0, c0 + 1]
                        .into_iter()
                        .filter(|p| p.abs() <= MAX_PULSE)
                        .min_by(|&a, &b| {
                            let cost = |p: i32| {
                                (recon(p) - target).powi(2) + pulse_lambda * p.abs() as f32
                            };
                            cost(a).partial_cmp(&cost(b)).unwrap()
                        })
                        .unwrap();
                    pulses[idx] = pl;
                    seed = seed.wrapping_add(pl);
                    let exc = if flip { -recon(pl) } else { recon(pl) };
                    r[p] = g * exc + pred;
                    y[p] = r[p] + lpc;
                    d[p] = y[p] - x[idx];
                    gq[p] = d[p] - s;
                    u[p] = d[p]
                        - c.iter()
                            .enumerate()
                            .map(|(j, &cj)| cj * d[p - 1 - j])
                            .sum::<f32>();
                    err += d[p] * d[p];
                }
            }
            Run {
                ltp_index,
                pulses,
                err,
                d,
                u,
                gq,
            }
        };

        let fs = self.fs_khz;
        // One full trial at a common gain offset (in quantiser steps): returns
        // the coded packet body so the rate loop can read its exact size.
        let candidates: Vec<usize> = if voiced { vec![0, 1, 2] } else { vec![0] };
        let mut trial = |step: i32| -> Trial {
            let scale = GAIN_STEP.powi(step);
            let mut t = targets;
            for v in t.iter_mut() {
                *v = ((*v as f64 * scale as f64).round() as i64).clamp(1, i32::MAX as i64) as i32;
            }
            let mut prev_gain_index = self.prev_gain_index;
            let (gain_idx, gains_q16) =
                quantize_gains(&t, &mut prev_gain_index, conditional, nb_subfr);
            let (per_index, run) = candidates
                .iter()
                .map(|&per| (per, run(per, &gains_q16)))
                .min_by(|a, b| a.1.err.partial_cmp(&b.1.err).unwrap())
                .unwrap();

            let mut trial = Trial {
                bytes: Vec::new(),
                range: 0,
                prev_gain_index,
                gain_idx,
                per_index,
                run,
                vad_flag,
                signal_type,
                quant_offset_type,
                wb,
                order,
                nb_subfr,
                fs,
                lag_index,
                contour_index,
                voiced,
                ltp_scale_index,
                nq: nq.clone(),
                ltp_gain,
                seed_index,
            };
            let enc = &mut self.range;
            enc.reset(1275);
            write_packet_header(enc, &[&trial]);
            write_frame_data(enc, &trial, conditional, prev_signal_type);
            enc.done();
            debug_assert!(!enc.error());
            let len = enc.range_bytes();
            trial.bytes = enc.data()[..len].to_vec();
            trial.range = enc.range();
            trial
        };

        // Rate control: a common gain offset moves every subframe's pulse
        // magnitudes together; the reservoir carries each packet's miss.
        let mut best = trial(if CARRY_RATE_STEP { self.rate_step } else { 0 });
        if let Some(bps) = self.target_bps {
            let frame_bits = bps as f32 * frame_ms as f32 / 1000.0;
            // libopus (enc_API.c): only overspend is remembered
            // (`nBitsExceeded`, clamped 0..10000 bits) and it is repaid at
            // `frame_ms / BITRESERVOIR_DECAY_TIME_MS` per frame, never all
            // at once. Repaying a whole frame's debt in one frame starved
            // voiced frames to 14-16 B with gains[0] at the ceiling (lane
            // opus-silkq r2: 24 packets carried ~100% of the 12k error).
            let target = if LIBOPUS_RESERVOIR {
                // Credit is deliberately not banked: banking it (spend
                // 2x, then starve) brought the 12k bursts back (err_ratio
                // 3.2 -> 11.6 on the speech gate).
                // libopus then limits TargetRate_bps to MIN_TARGET_RATE_BPS.
                let t = frame_bits - self.reservoir.max(0.0) * frame_ms as f32 / BITRESERVOIR_DECAY_TIME_MS;
                if TARGET_FLOOR { t.max(MIN_TARGET_RATE_BPS * frame_ms as f32 / 1000.0) } else { t }
            } else {
                frame_bits + self.reservoir.clamp(-frame_bits, frame_bits)
            };
            let bits_of = |t: &Trial| t.bytes.len() as f32 * 8.0;
            let start = if CARRY_RATE_STEP { self.rate_step } else { 0 };
            let (mut s0, mut b0) = (start, bits_of(&best));
            // Each quantiser step is ~0.227 bit per sample; a second trial
            // corrects the slope from the first measurement.
            let mut slope = 0.227 * n as f32;
            for _ in 0..2 {
                let mut step = s0 + ((b0 - target) / slope).round() as i32;
                if LIBOPUS_RESERVOIR {
                    // libopus bounds gainMult_Q8 to 64..1024 (x0.25..x4).
                    step = step.clamp(start - MAX_GAIN_STEPS, start + MAX_GAIN_STEPS);
                }
                if step == s0 {
                    break;
                }
                let t = trial(step);
                let b1 = bits_of(&t);
                if (b1 - target).abs() < (b0 - target).abs() {
                    if b1 != b0 {
                        slope = ((b1 - b0) / (step - s0) as f32).abs().max(1.0);
                    }
                    best = t;
                    s0 = step;
                    b0 = b1;
                } else {
                    break;
                }
            }
            self.rate_ema += (s0 as f32 - self.rate_ema) * CARRY_EMA;
            self.rate_step = (self.rate_ema.round() as i32).clamp(-MAX_RATE_STEP, MAX_RATE_STEP);
            self.reservoir = if LIBOPUS_RESERVOIR {
                // Debt only (bits over the frame's share), as `nBitsExceeded`.
                (self.reservoir + b0 - frame_bits).clamp(0.0, MAX_BITS_EXCEEDED)
            } else {
                (self.reservoir + frame_bits - b0).clamp(
                    -RESERVOIR_FRAMES * frame_bits,
                    RESERVOIR_FRAMES * frame_bits,
                )
            };
        }
        if let Some(enc) = shared {
            let trial_ref = &best;
            write_packet_header(enc, &[trial_ref]);
            write_frame_data(enc, trial_ref, conditional, prev_signal_type);
        }
        self.prev_gain_index = best.prev_gain_index;
        self.shape_d.copy_from_slice(&best.run.d[n..]);
        self.shape_u.copy_from_slice(&best.run.u[n..]);
        self.shape_gq.copy_from_slice(&best.run.gq[n..]);
        self.last_diag = SilkFrameDiag {
            voiced: best.voiced,
            signal_type: best.signal_type,
            gain_idx: best.gain_idx,
            nb_subfr: best.nb_subfr,
            lag_index: best.lag_index,
            pitch_l: {
                let mut pl = [0i32; MAX_NB_SUBFR];
                for (i, &v) in pitch_l.iter().enumerate().take(best.nb_subfr) {
                    pl[i] = v;
                }
                pl
            },
            nlsf_interp: best.nq.interp_index,
            bytes: best.bytes.len(),
            ltp_gain: best.ltp_gain,
        };
        Ok(best)
    }
}

/// A stereo, 10 or 20 ms, NB/MB/WB SILK encoder.
#[derive(Clone, Debug)]
pub struct SilkStereoEncoder {
    fs_khz: usize,
    mid: SilkEncoder,
    side: SilkEncoder,
    range: RangeEncoder,
    mirror: SilkDecoder,
    final_range: u32,
    mirror_range: u32,
    mirror_out: Vec<i16>,
    target_bps: Option<u32>,
    prev_mid_only: bool,
    voiced_frames: usize,
}

impl SilkStereoEncoder {
    /// Narrowband or wideband stereo encoder.
    pub fn new(wideband: bool) -> SilkStereoEncoder {
        Self::with_fs_khz(if wideband { 16 } else { 8 })
    }

    /// Mediumband stereo encoder.
    pub fn new_mediumband() -> SilkStereoEncoder {
        Self::with_fs_khz(12)
    }

    fn with_fs_khz(fs_khz: usize) -> SilkStereoEncoder {
        SilkStereoEncoder {
            fs_khz,
            mid: SilkEncoder::with_fs_khz(fs_khz),
            side: SilkEncoder::with_fs_khz(fs_khz),
            range: RangeEncoder::new(),
            mirror: SilkDecoder::new(fs_khz as u32 * 1000, 2),
            final_range: 0,
            mirror_range: 0,
            mirror_out: vec![0; 2 * 20 * fs_khz],
            target_bps: None,
            prev_mid_only: false,
            voiced_frames: 0,
        }
    }

    /// The range coder state after the last packet.
    pub fn final_range(&self) -> u32 {
        self.final_range
    }

    /// Packets so far coded with a long-term predictor in either channel.
    pub fn voiced_frames(&self) -> usize {
        self.voiced_frames
    }

    /// Target bit rate in bits per second for the whole stereo packet.
    pub fn set_bitrate(&mut self, bps: u32) {
        self.target_bps = Some(bps);
        // libopus (stereo_LR_to_MS) gives the side channel a fraction of
        // the total that shrinks with its energy; a flat half starved the
        // mid at 16 kbps stereo (libopus-heard corr .34 on the rate table).
        let mid = (bps * 2 / 3).max(500);
        let side = (bps - mid).max(500);
        self.mid.set_bitrate(mid);
        self.side.set_bitrate(side);
    }

    /// Algorithmic delay in 48 kHz samples.
    pub fn delay_samples(&self) -> usize {
        self.mid.delay_samples()
    }

    /// The SILK layer of a hybrid packet: codes one interleaved stereo 48 kHz
    /// frame into the packet's shared range coder without finishing it.
    pub fn encode_hybrid_ms(
        &mut self,
        pcm48: &[f32],
        enc: &mut RangeEncoder,
        frame_ms: usize,
    ) -> Result<()> {
        assert!(
            matches!(frame_ms, 10 | 20),
            "SilkStereoEncoder codes 10 or 20 ms stereo frames"
        );
        assert_eq!(
            pcm48.len(),
            2 * 48 * frame_ms,
            "SilkStereoEncoder input is 48 kHz stereo"
        );
        if self.prev_mid_only {
            self.side.reset_state();
            if let Some(bps) = self.target_bps {
                self.side.set_bitrate((bps / 2).max(500));
            }
        }
        let samples = 48 * frame_ms;
        let mut mid = Vec::with_capacity(samples);
        let mut side = Vec::with_capacity(samples);
        let mut side_energy = 0.0f32;
        for lr in pcm48.chunks_exact(2) {
            let m = 0.5 * (lr[0] + lr[1]);
            let s = 0.5 * (lr[0] - lr[1]);
            mid.push(m);
            side.push(s);
            side_energy += s * s;
        }
        let mid_trial = self
            .mid
            .encode_inner(&mid, frame_ms, None, false, 0, true)?;
        let side_trial = self
            .side
            .encode_inner(&side, frame_ms, None, false, 0, true)?;
        let mid_only = side_energy < 1.0e-10;
        write_stereo_packet_header(enc, &mid_trial, &side_trial, mid_only);
        write_stereo_zero_pred(enc);
        if !side_trial.vad_flag {
            enc.enc_icdf(usize::from(mid_only), &STEREO_ONLY_CODE_MID_ICDF, 8);
        }
        write_frame_data(enc, &mid_trial, false, 0);
        if !mid_only {
            write_frame_data(enc, &side_trial, false, 0);
        }
        self.mid.replay_ms(&mid_trial.bytes, frame_ms)?;
        if mid_only {
            self.side.reset_state();
        } else {
            self.side.replay_ms(&side_trial.bytes, frame_ms)?;
        }
        self.prev_mid_only = mid_only;
        self.voiced_frames += usize::from(mid_trial.voiced || side_trial.voiced);
        Ok(())
    }

    /// Encodes one 10 or 20 ms interleaved stereo frame at 48 kHz.
    pub fn encode_frame_ms(
        &mut self,
        pcm48: &[f32],
        out: &mut [u8],
        frame_ms: usize,
    ) -> Result<usize> {
        assert!(
            matches!(frame_ms, 10 | 20),
            "SilkStereoEncoder codes 10 or 20 ms stereo frames"
        );
        assert_eq!(
            pcm48.len(),
            2 * 48 * frame_ms,
            "SilkStereoEncoder input is 48 kHz stereo"
        );
        if self.prev_mid_only {
            self.side.reset_state();
            if let Some(bps) = self.target_bps {
                self.side.set_bitrate((bps / 2).max(500));
            }
        }
        let samples = 48 * frame_ms;
        let mut mid = Vec::with_capacity(samples);
        let mut side = Vec::with_capacity(samples);
        let mut side_energy = 0.0f32;
        for lr in pcm48.chunks_exact(2) {
            let m = 0.5 * (lr[0] + lr[1]);
            let s = 0.5 * (lr[0] - lr[1]);
            mid.push(m);
            side.push(s);
            side_energy += s * s;
        }
        let mid_trial = self
            .mid
            .encode_inner(&mid, frame_ms, None, false, 0, true)?;
        let side_trial = self
            .side
            .encode_inner(&side, frame_ms, None, false, 0, true)?;
        let mid_only = side_energy < 1.0e-10;

        self.range.reset(1275);
        write_stereo_packet_header(&mut self.range, &mid_trial, &side_trial, mid_only);
        write_stereo_zero_pred(&mut self.range);
        if !side_trial.vad_flag {
            self.range
                .enc_icdf(usize::from(mid_only), &STEREO_ONLY_CODE_MID_ICDF, 8);
        }
        write_frame_data(&mut self.range, &mid_trial, false, 0);
        if !mid_only {
            write_frame_data(&mut self.range, &side_trial, false, 0);
        }
        self.range.done();
        debug_assert!(!self.range.error());
        let len = self.range.range_bytes();
        self.final_range = self.range.range();
        let duration = if frame_ms == 10 { 0u8 } else { 1 };
        let base = match self.fs_khz {
            8 => 0u8,
            12 => 4,
            _ => 8,
        };
        if out.len() < 1 + len {
            return Err(ec_core::Error::corrupt(format!(
                "silk stereo encode: packet needs {} bytes, buffer holds {}",
                1 + len,
                out.len()
            )));
        }
        out[0] = ((base + duration) << 3) | 0x04;
        out[1..1 + len].copy_from_slice(&self.range.data()[..len]);
        self.replay_ms(&out[1..1 + len], frame_ms)?;
        self.mid.replay_ms(&mid_trial.bytes, frame_ms)?;
        if mid_only {
            self.side.reset_state();
        } else {
            self.side.replay_ms(&side_trial.bytes, frame_ms)?;
        }
        self.prev_mid_only = mid_only;
        self.voiced_frames += usize::from(mid_trial.voiced || side_trial.voiced);
        debug_assert_eq!(self.mirror_range, self.final_range);
        Ok(1 + len)
    }

    /// Decodes a finished hybrid payload (TOC stripped) through the stereo
    /// mirror so the range state can be checked against the packet encoder.
    pub fn replay_hybrid(&mut self, payload: &[u8], frame_ms: usize) -> Result<()> {
        self.replay_ms(payload, frame_ms)
    }

    fn replay_ms(&mut self, payload: &[u8], frame_ms: usize) -> Result<()> {
        let fs = self.fs_khz;
        let mut dec = RangeDecoder::new(payload);
        let total = 2 * frame_ms * fs;
        if self.mirror_out.len() < total {
            self.mirror_out.resize(total, 0);
        }
        let n = self.mirror.decode(
            &mut dec,
            &mut self.mirror_out[..total],
            frame_ms,
            fs as u32 * 1000,
            2,
            true,
        )?;
        debug_assert_eq!(n, frame_ms * fs);
        self.mirror_range = dec.range();
        Ok(())
    }
}

fn write_stereo_packet_header(enc: &mut RangeEncoder, mid: &Trial, side: &Trial, mid_only: bool) {
    enc.enc_bit_logp(mid.vad_flag, 1);
    enc.enc_bit_logp(false, 1);
    enc.enc_bit_logp(if mid_only { false } else { side.vad_flag }, 1);
    enc.enc_bit_logp(false, 1);
}

fn write_stereo_zero_pred(enc: &mut RangeEncoder) {
    enc.enc_icdf(12, &STEREO_PRED_JOINT_ICDF, 8);
    enc.enc_icdf(1, &UNIFORM3_ICDF, 8);
    enc.enc_icdf(2, &UNIFORM5_ICDF, 8);
    enc.enc_icdf(1, &UNIFORM3_ICDF, 8);
    enc.enc_icdf(2, &UNIFORM5_ICDF, 8);
}
/// One closed-loop quantisation of a frame.
struct Run {
    ltp_index: [usize; MAX_NB_SUBFR],
    pulses: Vec<i32>,
    err: f32,
    d: Vec<f32>,
    u: Vec<f32>,
    gq: Vec<f32>,
}

struct Trial {
    bytes: Vec<u8>,
    range: u32,
    prev_gain_index: i32,
    gain_idx: [i8; MAX_NB_SUBFR],
    per_index: usize,
    run: Run,
    vad_flag: bool,
    signal_type: i32,
    quant_offset_type: usize,
    wb: bool,
    order: usize,
    nb_subfr: usize,
    fs: usize,
    lag_index: i32,
    contour_index: i32,
    voiced: bool,
    ltp_scale_index: usize,
    nq: NlsfQuant,
    ltp_gain: f32,
    seed_index: i32,
}

fn write_packet_header(enc: &mut RangeEncoder, frames: &[&Trial]) {
    for frame in frames {
        enc.enc_bit_logp(frame.vad_flag, 1);
    }
    enc.enc_bit_logp(false, 1);
}

fn write_frame_data(
    enc: &mut RangeEncoder,
    frame: &Trial,
    conditional: bool,
    prev_signal_type: i32,
) {
    if frame.vad_flag {
        enc.enc_icdf(
            ((frame.signal_type << 1) | frame.quant_offset_type as i32) as usize - 2,
            &TYPE_OFFSET_VAD_ICDF,
            8,
        );
    } else {
        enc.enc_icdf(frame.quant_offset_type, &TYPE_OFFSET_NO_VAD_ICDF, 8);
    }
    if conditional {
        enc.enc_icdf(frame.gain_idx[0] as usize, &DELTA_GAIN_ICDF, 8);
    } else {
        enc.enc_icdf(
            (frame.gain_idx[0] as usize) >> 3,
            &GAIN_ICDF[frame.signal_type as usize],
            8,
        );
        enc.enc_icdf((frame.gain_idx[0] as usize) & 7, &UNIFORM8_ICDF, 8);
    }
    for k in 1..frame.nb_subfr {
        enc.enc_icdf(frame.gain_idx[k] as usize, &DELTA_GAIN_ICDF, 8);
    }
    let cb = nlsf_cb(frame.wb);
    let stage1_icdf = &cb.cb1_icdf[(frame.signal_type as usize >> 1) * cb.n_vectors..];
    enc.enc_icdf(frame.nq.indices[0] as usize, stage1_icdf, 8);
    let (ec_ix, _) = crate::silk::nlsf_unpack(cb, frame.nq.indices[0] as usize);
    for i in 0..frame.order {
        let q = frame.nq.indices[i + 1] as i32;
        let icdf = &cb.ec_icdf[ec_ix[i] as usize..];
        if q <= -4 {
            enc.enc_icdf(0, icdf, 8);
            enc.enc_icdf((-4 - q) as usize, &NLSF_EXT_ICDF, 8);
        } else if q >= 4 {
            enc.enc_icdf(8, icdf, 8);
            enc.enc_icdf((q - 4) as usize, &NLSF_EXT_ICDF, 8);
        } else {
            enc.enc_icdf((q + 4) as usize, icdf, 8);
        }
    }
    if frame.nb_subfr == MAX_NB_SUBFR {
        enc.enc_icdf(
            frame.nq.interp_index as usize,
            &NLSF_INTERPOLATION_FACTOR_ICDF,
            8,
        );
    }
    if frame.voiced {
        if conditional && prev_signal_type == 2 {
            enc.enc_icdf(0, &PITCH_DELTA_ICDF, 8);
        }
        let half = frame.fs as i32 >> 1;
        enc.enc_icdf((frame.lag_index / half) as usize, &PITCH_LAG_ICDF, 8);
        let low_icdf: &[u8] = match frame.fs {
            16 => &UNIFORM8_ICDF,
            12 => &UNIFORM6_ICDF,
            _ => &UNIFORM4_ICDF,
        };
        enc.enc_icdf((frame.lag_index % half) as usize, low_icdf, 8);
        let contour_icdf: &[u8] = match (frame.fs, frame.nb_subfr) {
            (8, 4) => &PITCH_CONTOUR_NB_ICDF,
            (8, _) => &PITCH_CONTOUR_10_MS_NB_ICDF,
            (_, 4) => &PITCH_CONTOUR_ICDF,
            _ => &PITCH_CONTOUR_10_MS_ICDF,
        };
        enc.enc_icdf(frame.contour_index as usize, contour_icdf, 8);
        enc.enc_icdf(frame.per_index, &LTP_PER_INDEX_ICDF, 8);
        let gain_icdf: &[u8] = match frame.per_index {
            0 => &LTP_GAIN_ICDF_0,
            1 => &LTP_GAIN_ICDF_1,
            _ => &LTP_GAIN_ICDF_2,
        };
        for k in 0..frame.nb_subfr {
            enc.enc_icdf(frame.run.ltp_index[k], gain_icdf, 8);
        }
        if !conditional {
            enc.enc_icdf(frame.ltp_scale_index, &LTPSCALE_ICDF, 8);
        }
    }
    enc.enc_icdf(frame.seed_index as usize, &UNIFORM4_ICDF, 8);
    encode_pulses(
        enc,
        &frame.run.pulses,
        frame.signal_type,
        frame.quant_offset_type as i32,
    );
}

/// Bits an ICDF symbol costs, for choosing among equivalent codings.
fn icdf_bits(k: usize, icdf: &[u8]) -> f32 {
    let hi = if k == 0 { 256 } else { icdf[k - 1] as u32 };
    (256.0 / (hi - icdf[k] as u32) as f32).log2()
}

/// Inverse of `decode_pulses`: rate level, shell-coded block sums with the
/// LSB escape, LSBs, signs.
fn encode_pulses(enc: &mut RangeEncoder, pulses: &[i32], signal_type: i32, quant_offset_type: i32) {
    let iter = pulses.len().div_ceil(SHELL_LEN);
    let mut abs = vec![0i32; iter * SHELL_LEN];
    for (a, &p) in abs.iter_mut().zip(pulses) {
        *a = p.abs();
    }
    let mut sums = vec![0i32; iter];
    let mut shifts = vec![0u32; iter];
    for i in 0..iter {
        let block = &abs[i * SHELL_LEN..(i + 1) * SHELL_LEN];
        let mut s = block.iter().sum::<i32>();
        while s > 16 {
            shifts[i] += 1;
            s = block.iter().map(|&a| a >> shifts[i]).sum();
        }
        sums[i] = s;
        debug_assert!(shifts[i] <= 10);
    }
    // Rate level: whichever table codes these block sums cheapest.
    let rate_level = (0..9)
        .min_by(|&a, &b| {
            let cost = |rl: usize| -> f32 {
                (0..iter)
                    .map(|i| {
                        let sym = if shifts[i] > 0 { 17 } else { sums[i] as usize };
                        icdf_bits(sym, &PULSES_PER_BLOCK_ICDF[rl])
                    })
                    .sum::<f32>()
                    + icdf_bits(rl, &RATE_LEVELS_ICDF[signal_type as usize >> 1])
            };
            cost(a).partial_cmp(&cost(b)).unwrap()
        })
        .unwrap();
    enc.enc_icdf(rate_level, &RATE_LEVELS_ICDF[signal_type as usize >> 1], 8);
    for i in 0..iter {
        let first = if shifts[i] > 0 { 17 } else { sums[i] as usize };
        enc.enc_icdf(first, &PULSES_PER_BLOCK_ICDF[rate_level], 8);
        for level in 1..=shifts[i] {
            let icdf = &PULSES_PER_BLOCK_ICDF[9][usize::from(level == 10)..];
            let sym = if level < shifts[i] {
                17
            } else {
                sums[i] as usize
            };
            enc.enc_icdf(sym, icdf, 8);
        }
    }
    for i in 0..iter {
        if sums[i] > 0 {
            let block: Vec<i32> = abs[i * SHELL_LEN..(i + 1) * SHELL_LEN]
                .iter()
                .map(|&a| a >> shifts[i])
                .collect();
            shell_encode(enc, &block);
        }
    }
    for i in 0..iter {
        if shifts[i] > 0 {
            for &a in &abs[i * SHELL_LEN..(i + 1) * SHELL_LEN] {
                for bit in (0..shifts[i]).rev() {
                    enc.enc_icdf(((a >> bit) & 1) as usize, &LSB_ICDF, 8);
                }
            }
        }
    }
    let base = 7 * (quant_offset_type + (signal_type << 1)) as usize;
    for i in 0..iter {
        let p = sums[i] | ((shifts[i] as i32) << 5);
        if p > 0 {
            let icdf = [SIGN_ICDF[base + (p & 0x1F).min(6) as usize], 0];
            for j in 0..SHELL_LEN {
                let idx = i * SHELL_LEN + j;
                if idx < pulses.len() && pulses[idx] != 0 {
                    enc.enc_icdf(usize::from(pulses[idx] > 0), &icdf, 8);
                }
            }
        }
    }
}

/// Inverse of `shell_decode`: the same binary split tree, written top down.
fn shell_encode(enc: &mut RangeEncoder, p: &[i32]) {
    fn join(enc: &mut RangeEncoder, a: i32, b: i32, table: &[u8]) -> i32 {
        let sum = a + b;
        if sum > 0 {
            enc.enc_icdf(
                a as usize,
                &table[SHELL_CODE_TABLE_OFFSETS[sum as usize] as usize..],
                8,
            );
        }
        sum
    }
    let p1: Vec<i32> = (0..8).map(|i| p[2 * i] + p[2 * i + 1]).collect();
    let p2: Vec<i32> = (0..4).map(|i| p1[2 * i] + p1[2 * i + 1]).collect();
    let p3: Vec<i32> = (0..2).map(|i| p2[2 * i] + p2[2 * i + 1]).collect();
    join(enc, p3[0], p3[1], &SHELL_CODE_TABLE3);
    join(enc, p2[0], p2[1], &SHELL_CODE_TABLE2);
    join(enc, p1[0], p1[1], &SHELL_CODE_TABLE1);
    join(enc, p[0], p[1], &SHELL_CODE_TABLE0);
    join(enc, p[2], p[3], &SHELL_CODE_TABLE0);
    join(enc, p1[2], p1[3], &SHELL_CODE_TABLE1);
    join(enc, p[4], p[5], &SHELL_CODE_TABLE0);
    join(enc, p[6], p[7], &SHELL_CODE_TABLE0);
    join(enc, p2[2], p2[3], &SHELL_CODE_TABLE2);
    join(enc, p1[4], p1[5], &SHELL_CODE_TABLE1);
    join(enc, p[8], p[9], &SHELL_CODE_TABLE0);
    join(enc, p[10], p[11], &SHELL_CODE_TABLE0);
    join(enc, p1[6], p1[7], &SHELL_CODE_TABLE1);
    join(enc, p[12], p[13], &SHELL_CODE_TABLE0);
    join(enc, p[14], p[15], &SHELL_CODE_TABLE0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pulse pattern the shell coder can see — including the LSB
    /// escape and a block that needs several shifts — survives a round trip
    /// through the decoder's own `decode_pulses` path via a full packet.
    #[test]
    fn pulses_roundtrip_through_the_decoder() {
        for (wb, seed) in [(false, 1u32), (true, 7u32), (false, 99u32)] {
            let mut enc = SilkEncoder::new(wb);
            let mut dec = SilkDecoder::new(if wb { 16000 } else { 8000 }, 1);
            let mut out = vec![0u8; 1500];
            let mut pcm = vec![0i16; 320];
            let mut s = seed;
            for frame in 0..6 {
                let x: Vec<f32> = (0..960)
                    .map(|i| {
                        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                        let noise = (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
                        // A loud click to force the LSB escape.
                        let click = if frame == 2 && i == 400 { 0.9 } else { 0.0 };
                        noise * 0.2 + click
                    })
                    .collect();
                let len = enc.encode_frame(&x, &mut out).unwrap();
                let mut rd = RangeDecoder::new(&out[1..len]);
                let n = dec
                    .decode(
                        &mut rd,
                        &mut pcm,
                        20,
                        if wb { 16000 } else { 8000 },
                        1,
                        true,
                    )
                    .unwrap();
                assert_eq!(n, if wb { 320 } else { 160 });
                assert_eq!(rd.range(), enc.final_range(), "frame {frame}");
                assert_eq!(&pcm[..n], &enc.mirror_out[..n]);
            }
        }
    }
}

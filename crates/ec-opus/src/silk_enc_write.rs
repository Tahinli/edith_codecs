//! SILK payload writer (subtask D2a): turns the analysis in [`crate::silk_enc`]
//! into a decodable mono 20 ms SILK packet, NB (8 kHz) or WB (16 kHz).
//!
//! Symbol order is exactly `silk::decode()`'s, and every decision is made
//! against the decoder's own tables and reconstruction functions. The
//! encoder keeps a [`SilkDecoder`] of its own and feeds it every packet it
//! writes, so the synthesis history its LTP search and residual computation
//! use *is* the real decoder's state — there is no encoder-side model of the
//! decoder that could drift.
//!
//! The excitation quantiser is deliberately simple (open-loop LPC residual,
//! analysis-by-synthesis LTP codebook pick, nearest-level pulse rounding
//! under the decoder's seed/offset convention). Noise shaping (NSQ), 10/40/60
//! ms frames and stereo are subtask D2b.

use ec_core::Result;

use crate::range::{RangeDecoder, RangeEncoder};
use crate::silk::tables::*;
use crate::silk::{
    decode_pitch, nlsf2a, nlsf_cb, silk_rand, SilkDecoder, MAX_LPC_ORDER, MAX_NB_SUBFR,
    PE_MAX_LAG_MS, PE_MIN_LAG_MS,
};
use crate::silk_enc::{estimate_pitch, lpc_analyze, quantize_gains, quantize_nlsf, Resampler48, Vad};

const NB_SUBFR: usize = 4;
const SHELL_LEN: usize = 16;
const LTP_ORDER: usize = 5;
/// `QUANT_LEVEL_ADJUST_Q10` in `decode_core`: a non-zero pulse decodes 80/1024
/// closer to zero than its integer value.
const LEVEL_ADJUST: f32 = 80.0 / 1024.0;
/// Largest pulse magnitude the shell coder's 10-level LSB escape can carry
/// with 16 such pulses in one block.
const MAX_PULSE: i32 = 1023;

/// A mono, 20 ms, NB or WB SILK encoder.
pub struct SilkEncoder {
    fs_khz: usize,
    resampler: Resampler48,
    vad: Vad,
    /// Replays every packet this encoder writes: its state is the decoder's.
    mirror: SilkDecoder,
    range: RangeEncoder,
    prev_gain_index: i32,
    final_range: u32,
    voiced_frames: usize,
    mirror_out: Vec<i16>,
}

impl SilkEncoder {
    /// `wideband`: 16 kHz internal rate (TOC config 9), else 8 kHz (config 1).
    pub fn new(wideband: bool) -> SilkEncoder {
        let fs_khz = if wideband { 16 } else { 8 };
        SilkEncoder {
            fs_khz,
            resampler: Resampler48::new(fs_khz as u32),
            vad: Vad::new(),
            mirror: SilkDecoder::new(fs_khz as u32 * 1000, 1),
            range: RangeEncoder::new(),
            prev_gain_index: 10,
            final_range: 0,
            voiced_frames: 0,
            mirror_out: vec![0; 20 * 16],
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

    /// Algorithmic delay in 48 kHz samples: the analysis resampler only
    /// (the decoder adds its own resampler delay on the way back up).
    pub fn delay_samples(&self) -> usize {
        self.resampler.delay_samples()
    }

    /// Encodes 960 mono 48 kHz samples as one code-0 packet in `out`,
    /// returning its length.
    pub fn encode_frame(&mut self, pcm48: &[f32], out: &mut [u8]) -> Result<usize> {
        assert_eq!(pcm48.len(), 960, "SilkEncoder codes 20 ms mono frames");
        let fs = self.fs_khz;
        let order = if fs == 16 { MAX_LPC_ORDER } else { 10 };
        let n = 20 * fs;
        let sub = 5 * fs;
        let x: Vec<f32> = self
            .resampler
            .process(pcm48)
            .iter()
            .map(|v| v * 32768.0)
            .collect();
        let vad = self.vad.analyze(&x);

        // LPC -> NLSF indices -> the decoder's dequantised filter.
        let nlsf_target = lpc_analyze(&x, order).map(|a| a.nlsf_q15).unwrap_or_else(|| {
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
        let pitch = estimate_pitch(&x, fs as i32, NB_SUBFR).filter(|p| p.voiced);
        let voiced = pitch.is_some();
        let vad_flag = vad.active || voiced;
        let signal_type: i32 = if !vad_flag { 0 } else if voiced { 2 } else { 1 };
        let quant_offset_type = 0usize;
        let offset = QUANTIZATION_OFFSETS_Q10[signal_type as usize >> 1][quant_offset_type] as f32
            / 1024.0;

        let ltp_scale_index = 0usize;
        let ltp_scale = LTPSCALES_TABLE_Q14[ltp_scale_index] as f32 / 16384.0;
        // `r`: LTP history in absolute sample units, past frame first (scaled
        // by ltp_scale as the decoder scales its rebuilt history), then this
        // frame's reconstructed residual as it is quantised.
        let mut r: Vec<f32> = (-(n as isize)..0).map(|i| whiten(i) * ltp_scale).collect();
        r.resize(2 * n, 0.0);

        let (lag_index, contour_index, pitch_l) = match &pitch {
            Some(p) => {
                let max_index = (PE_MAX_LAG_MS - PE_MIN_LAG_MS) * fs as i32 - 1;
                let li = p.lag_index.clamp(0, max_index);
                (li, p.contour_index, decode_pitch(li, p.contour_index, fs as i32, NB_SUBFR))
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
        // Codebook pick by min squared error, open loop (the frame's own
        // residual stands in for the not-yet-quantised part of the history).
        let mut per_index = 0usize;
        let mut ltp_index = [0usize; MAX_NB_SUBFR];
        if voiced {
            let mut r_ol = r.clone();
            r_ol[n..].copy_from_slice(&res);
            let mut best_total = f32::INFINITY;
            for (per, cbk) in [&LTP_GAIN_VQ_0[..], &LTP_GAIN_VQ_1[..], &LTP_GAIN_VQ_2[..]]
                .iter()
                .enumerate()
            {
                let mut total = 0f32;
                let mut idx = [0usize; MAX_NB_SUBFR];
                for k in 0..NB_SUBFR {
                    let mut best = f32::INFINITY;
                    for (v, b) in cbk.iter().enumerate() {
                        let err: f32 = (0..sub)
                            .map(|i| {
                                let d = res[k * sub + i] - ltp_pred(&r_ol, b, k, i);
                                d * d
                            })
                            .sum();
                        if err < best {
                            best = err;
                            idx[k] = v;
                        }
                    }
                    total += best;
                }
                if total < best_total {
                    best_total = total;
                    per_index = per;
                    ltp_index = idx;
                }
            }
        }
        let cbk: &[[i8; 5]] = match per_index {
            0 => &LTP_GAIN_VQ_0,
            1 => &LTP_GAIN_VQ_1,
            _ => &LTP_GAIN_VQ_2,
        };

        // Gains: the open-loop excitation RMS per subframe, so pulses land
        // around unit magnitude.
        let mut targets = [0i32; MAX_NB_SUBFR];
        for k in 0..NB_SUBFR {
            let mut r_ol = r.clone();
            r_ol[n..].copy_from_slice(&res);
            let e: f32 = (0..sub)
                .map(|i| {
                    let p = if voiced { ltp_pred(&r_ol, &cbk[ltp_index[k]], k, i) } else { 0.0 };
                    let d = res[k * sub + i] - p;
                    d * d
                })
                .sum::<f32>()
                / sub as f32;
            targets[k] = ((e.sqrt() * 65536.0).round() as i64).clamp(1, i32::MAX as i64) as i32;
        }
        let (gain_idx, gains_q16) =
            quantize_gains(&targets, &mut self.prev_gain_index, false, NB_SUBFR);

        // Pulses: sequential, under the decoder's seed-driven sign flips.
        let seed_index = 0i32;
        let mut seed = seed_index;
        let mut pulses = vec![0i32; n];
        for k in 0..NB_SUBFR {
            let g = gains_q16[k] as f32 / 65536.0;
            for i in 0..sub {
                let idx = k * sub + i;
                let pred = if voiced { ltp_pred(&r, &cbk[ltp_index[k]], k, i) } else { 0.0 };
                let e = (res[idx] - pred) / g;
                seed = silk_rand(seed);
                let flip = seed < 0;
                let target = if flip { -e } else { e };
                let recon = |p: i32| -> f32 {
                    let lvl = p as f32 - LEVEL_ADJUST * (p.signum() as f32);
                    lvl + offset
                };
                let c = (target.round() as i32).clamp(-MAX_PULSE, MAX_PULSE);
                let p = [c - 1, c, c + 1]
                    .into_iter()
                    .filter(|p| p.abs() <= MAX_PULSE)
                    .min_by(|&a, &b| {
                        (recon(a) - target)
                            .abs()
                            .partial_cmp(&(recon(b) - target).abs())
                            .unwrap()
                    })
                    .unwrap();
                pulses[idx] = p;
                seed = seed.wrapping_add(p);
                let exc = if flip { -recon(p) } else { recon(p) };
                r[n + idx] = g * (exc + pred);
            }
        }

        // --- Bitstream, in decode()'s order. ---
        let enc = &mut self.range;
        enc.reset(1275);
        enc.enc_bit_logp(vad_flag, 1); // VAD flag
        enc.enc_bit_logp(false, 1); // LBRR flag
        // Frame type.
        if vad_flag {
            enc.enc_icdf(((signal_type << 1) | quant_offset_type as i32) as usize - 2, &TYPE_OFFSET_VAD_ICDF, 8);
        } else {
            enc.enc_icdf(quant_offset_type, &TYPE_OFFSET_NO_VAD_ICDF, 8);
        }
        // Gains: one frame per packet, so always independently coded.
        enc.enc_icdf((gain_idx[0] as usize) >> 3, &GAIN_ICDF[signal_type as usize], 8);
        enc.enc_icdf((gain_idx[0] as usize) & 7, &UNIFORM8_ICDF, 8);
        for k in 1..NB_SUBFR {
            enc.enc_icdf(gain_idx[k] as usize, &DELTA_GAIN_ICDF, 8);
        }
        // NLSFs.
        let cb = nlsf_cb(wb);
        let stage1_icdf = &cb.cb1_icdf[(signal_type as usize >> 1) * cb.n_vectors..];
        enc.enc_icdf(nq.indices[0] as usize, stage1_icdf, 8);
        let (ec_ix, _) = crate::silk::nlsf_unpack(cb, nq.indices[0] as usize);
        for i in 0..order {
            let q = nq.indices[i + 1] as i32;
            let icdf = &cb.ec_icdf[ec_ix[i] as usize..];
            if q < -4 {
                enc.enc_icdf(0, icdf, 8);
                enc.enc_icdf((-4 - q) as usize, &NLSF_EXT_ICDF, 8);
            } else if q > 4 {
                enc.enc_icdf(8, icdf, 8);
                enc.enc_icdf((q - 4) as usize, &NLSF_EXT_ICDF, 8);
            } else {
                enc.enc_icdf((q + 4) as usize, icdf, 8);
            }
        }
        enc.enc_icdf(nq.interp_index as usize, &NLSF_INTERPOLATION_FACTOR_ICDF, 8);
        // Pitch + LTP.
        if voiced {
            let half = fs as i32 >> 1;
            enc.enc_icdf((lag_index / half) as usize, &PITCH_LAG_ICDF, 8);
            let low_icdf: &[u8] = if wb { &UNIFORM8_ICDF } else { &UNIFORM4_ICDF };
            enc.enc_icdf((lag_index % half) as usize, low_icdf, 8);
            let contour_icdf: &[u8] = if wb { &PITCH_CONTOUR_ICDF } else { &PITCH_CONTOUR_NB_ICDF };
            enc.enc_icdf(contour_index as usize, contour_icdf, 8);
            enc.enc_icdf(per_index, &LTP_PER_INDEX_ICDF, 8);
            let gain_icdf: &[u8] = match per_index {
                0 => &LTP_GAIN_ICDF_0,
                1 => &LTP_GAIN_ICDF_1,
                _ => &LTP_GAIN_ICDF_2,
            };
            for k in 0..NB_SUBFR {
                enc.enc_icdf(ltp_index[k], gain_icdf, 8);
            }
            enc.enc_icdf(ltp_scale_index, &LTPSCALE_ICDF, 8);
            self.voiced_frames += 1;
        }
        enc.enc_icdf(seed_index as usize, &UNIFORM4_ICDF, 8);
        encode_pulses(enc, &pulses, signal_type, quant_offset_type as i32);
        enc.done();
        debug_assert!(!enc.error());
        let len = enc.range_bytes();
        self.final_range = enc.range();

        let toc = if wb { 9u8 << 3 } else { 1u8 << 3 };
        if out.len() < 1 + len {
            return Err(ec_core::Error::corrupt(format!(
                "silk encode: packet needs {} bytes, buffer holds {}",
                1 + len,
                out.len()
            )));
        }
        out[0] = toc;
        out[1..1 + len].copy_from_slice(&enc.data()[..len]);

        // Replay through the mirror so the next frame sees decoder state.
        let mut dec = RangeDecoder::new(&out[1..1 + len]);
        self.mirror
            .decode(&mut dec, &mut self.mirror_out, 20, fs as u32 * 1000, 1, true)?;
        debug_assert_eq!(dec.range(), self.final_range);
        Ok(1 + len)
    }
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
            let sym = if level < shifts[i] { 17 } else { sums[i] as usize };
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
            enc.enc_icdf(a as usize, &table[SHELL_CODE_TABLE_OFFSETS[sum as usize] as usize..], 8);
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
                    .decode(&mut rd, &mut pcm, 20, if wb { 16000 } else { 8000 }, 1, true)
                    .unwrap();
                assert_eq!(n, if wb { 320 } else { 160 });
                assert_eq!(rd.range(), enc.final_range(), "frame {frame}");
                assert_eq!(&pcm[..n], &enc.mirror_out[..n]);
            }
        }
    }
}

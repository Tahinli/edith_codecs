//! The CELT (MDCT) layer of an Opus *encoder* — the mirror of [`crate::celt`].
//!
//! Every symbol this writes is read back by [`crate::celt::CeltDecoder`] in the
//! same order, and both sides derive the bit allocation from the same
//! `tell_frac()`; the two files therefore share the tables, the allocation
//! ([`crate::celt::compute_allocation`]) and every helper whose result feeds a
//! coded decision. What is *not* shared is what an encoder alone decides, and
//! that is where the quality lives:
//!
//! - **transient / TF**: short blocks when the sub-block energy jumps, then a
//!   per-band time-frequency resolution chosen by an L1 sparsity search with a
//!   Viterbi pass over the toggle chain that codes it (`tf_analysis`);
//! - **dynalloc**: bands that stand above a spreading-masking follower get
//!   extra bits, bounded by a share of the frame (`dynalloc_analysis`);
//! - **trim**: the static allocation is tilted from the measured spectral tilt;
//! - **spread**: the PVQ rotation strength follows the measured tonality;
//! - **stereo**: per-band mid/side angle from the real energies, an intensity
//!   threshold from the per-band side energy, and dual-stereo when the two
//!   channels are uncorrelated enough that mid/side buys nothing;
//! - **PVQ**: a greedy pulse search on the rotated band followed by a
//!   single-pulse relocation pass, both maximising `(x.y)^2/(y.y)`.
//!
//! The forward MDCT is the exact adjoint of the decoder's inverse, scaled by
//! `2/L` so that analysis-synthesis is unity gain: perfect reconstruction is a
//! property of the code, not of a constant someone measured
//! (`mdct_round_trip_is_unity_gain` pins it).
//!
//! Encoder-side delay is one overlap, 120 samples at 48 kHz: frame `t` analyses
//! input `[t*N - 120, t*N + N)`, so the decoded stream lags the input by 120
//! samples and an Ogg-Opus pre-skip of 120 removes it exactly.

use ec_core::{Error, Result};

use crate::celt::{
    self, AllocArrays, AllocCoder, BandArgs, E_BANDS, E_MEANS, Fft15, LOG_N, Lowband, NB_BANDS,
    SkipCtx,
};
use crate::range_enc::RangeEncoder;

use celt::{
    BETA_COEF, BETA_INTRA, BITRES, CACHE_CAPS, E_PROB_MODEL, MAX_FINE_BITS, OVERLAP, PRED_COEF,
    PREEMPH, QTHETA_OFFSET, QTHETA_OFFSET_TWOPHASE, SHORT_MDCT, SIG_SCALE, SMALL_ENERGY_ICDF,
    SPREAD_AGGRESSIVE, SPREAD_ICDF, SPREAD_NONE, SPREAD_NORMAL, TF_SELECT, TRIM_ICDF, bits2pulses,
    cache_index, celt_lcg_rand, compute_qn, deinterleave_hadamard, exp_rotation, get_pulses, haar1,
    interleave_hadamard, pulses2bits, pvq_urow, renormalise, stereo_merge, uprev,
};

/// Longest frame in 48 kHz samples.
const MAX_FRAME: usize = 960;

// ---------------------------------------------------------------------------
// Forward MDCT
// ---------------------------------------------------------------------------

/// One forward-MDCT plan: `l` coefficients out of `l + OVERLAP` samples.
///
/// This is the transpose of [`crate::celt`]'s inverse plan, step for step, so
/// the two agree by construction rather than by a second derivation.
#[derive(Clone, Debug)]
struct MdctPlan {
    l: usize,
    fft: Fft15,
    rot_re: Vec<f32>,
    rot_im: Vec<f32>,
    fre: Vec<f32>,
    fim: Vec<f32>,
    t: Vec<f32>,
}

impl MdctPlan {
    fn new(l: usize) -> MdctPlan {
        let n = 2 * l;
        let quarter = l / 2;
        let mut rot_re = vec![0.0; quarter];
        let mut rot_im = vec![0.0; quarter];
        for i in 0..quarter {
            let a = 2.0 * core::f64::consts::PI * (i as f64 + 0.125) / n as f64;
            rot_re[i] = a.cos() as f32;
            rot_im[i] = a.sin() as f32;
        }
        MdctPlan {
            l,
            fft: Fft15::new(quarter),
            rot_re,
            rot_im,
            fre: vec![0.0; quarter],
            fim: vec![0.0; quarter],
            t: vec![0.0; l],
        }
    }

    /// Transforms `l + OVERLAP` windowed samples into `l` coefficients written
    /// at `out[j * stride]`.
    fn forward(&mut self, inp: &[f32], window: &[f32], out: &mut [f32], stride: usize) {
        let l = self.l;
        let quarter = l / 2;
        let flat = quarter - OVERLAP / 2;
        let mirror = quarter + OVERLAP / 2;
        let t = &mut self.t[..l];

        // Adjoint of the inverse's windowed scatter.
        for i in 0..flat {
            t[quarter - 1 - i] = inp[mirror - 1 - i];
            t[quarter + i] = inp[mirror + i];
        }
        for i in flat..quarter {
            let w = i - flat;
            t[quarter - 1 - i] =
                -window[w] * inp[w] + window[OVERLAP - 1 - w] * inp[mirror - 1 - i];
            t[quarter + i] =
                window[w] * inp[l + OVERLAP - 1 - w] + window[OVERLAP - 1 - w] * inp[mirror + i];
        }
        // Adjoint of the de-shuffle.
        for i in 0..quarter {
            self.fre[i] = -t[2 * i];
            self.fim[quarter - 1 - i] = t[2 * i + 1];
        }
        // Adjoint of the post-rotation: multiply by the conjugate.
        for i in 0..quarter {
            let (re, im) = (self.fre[i], self.fim[i]);
            let (c, s) = (self.rot_re[i], self.rot_im[i]);
            self.fre[i] = re * c + im * s;
            self.fim[i] = im * c - re * s;
        }
        // Adjoint of the unscaled inverse FFT is the unscaled forward one.
        for v in self.fim.iter_mut() {
            *v = -*v;
        }
        self.fft.inverse(&mut self.fre, &mut self.fim);
        for v in self.fim.iter_mut() {
            *v = -*v;
        }
        // Adjoint of the pre-rotation, and the 2/L that makes the round trip
        // through the decoder's inverse unity gain.
        let scale = 2.0 / l as f32;
        for i in 0..quarter {
            let (c, s) = (self.rot_re[i], self.rot_im[i]);
            let (re, im) = (self.fre[i], self.fim[i]);
            out[(2 * i) * stride] = scale * (s * re - c * im);
            out[(l - 1 - 2 * i) * stride] = scale * (-c * re - s * im);
        }
    }
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// A CELT layer encoder, one per Opus stream (mono or coupled stereo).
#[derive(Clone, Debug)]
pub struct CeltEncoder {
    channels: usize,
    /// Input decimation the caller feeds us: 1 at 48 kHz, 2 at 24 kHz...
    upsample: usize,
    window: Vec<f32>,
    plans: Vec<MdctPlan>,
    /// Pre-emphasised input history, `channels * OVERLAP`.
    hist: Vec<f32>,
    preemph_mem: [f32; 2],
    /// Quantised band energies, exactly what the decoder will hold.
    old_band_e: Vec<f32>,
    /// This frame's measured band energies (log2, mean removed).
    band_log_e: Vec<f32>,
    /// Coarse-quantisation residual, spent by the fine bits.
    err: Vec<f32>,
    scratch_old: Vec<f32>,
    scratch_err: Vec<f32>,
    /// The folding PRNG, carried between frames exactly as the decoder does.
    rng: u32,
    started: bool,
    consec_transient: u32,
    // Per-frame scratch, allocated once.
    work: Vec<f32>,
    freq: Vec<f32>,
    x: Vec<f32>,
    norm: Vec<f32>,
    lowband_scratch: Vec<f32>,
    hadamard_tmp: Vec<f32>,
    tf_tmp: Vec<f32>,
    xa: Vec<f32>,
    iy: Vec<i32>,
    urow: Vec<u32>,
    band_e: Vec<f32>,
    pulses: [i32; NB_BANDS],
    fine_quant: [i32; NB_BANDS],
    fine_priority: [i32; NB_BANDS],
    offsets: [i32; NB_BANDS],
    caps: [i32; NB_BANDS],
    tf_res: [i32; NB_BANDS],
    collapse_masks: [u8; 2 * NB_BANDS],
}

impl CeltEncoder {
    /// An encoder for `channels` channels fed at `48000/upsample` Hz.
    pub fn new(channels: usize, upsample: usize) -> CeltEncoder {
        assert!((1..=2).contains(&channels));
        let max_n = MAX_FRAME;
        CeltEncoder {
            channels,
            upsample,
            window: celt::overlap_window(),
            plans: (0..4).map(|lm| MdctPlan::new(SHORT_MDCT << lm)).collect(),
            hist: vec![0.0; channels * OVERLAP],
            preemph_mem: [0.0; 2],
            old_band_e: vec![0.0; 2 * NB_BANDS],
            band_log_e: vec![0.0; 2 * NB_BANDS],
            err: vec![0.0; 2 * NB_BANDS],
            scratch_old: vec![0.0; 2 * NB_BANDS],
            scratch_err: vec![0.0; 2 * NB_BANDS],
            rng: 0,
            started: false,
            consec_transient: 0,
            work: vec![0.0; 2 * (max_n + OVERLAP)],
            freq: vec![0.0; 2 * max_n],
            x: vec![0.0; 2 * max_n],
            norm: vec![0.0; 2 * max_n],
            lowband_scratch: vec![0.0; max_n],
            hadamard_tmp: vec![0.0; max_n],
            tf_tmp: vec![0.0; 2 * max_n],
            xa: vec![0.0; max_n],
            iy: vec![0; max_n],
            urow: vec![0; 512],
            band_e: vec![0.0; 2 * NB_BANDS],
            pulses: [0; NB_BANDS],
            fine_quant: [0; NB_BANDS],
            fine_priority: [0; NB_BANDS],
            offsets: [0; NB_BANDS],
            caps: [0; NB_BANDS],
            tf_res: [0; NB_BANDS],
            collapse_masks: [0; 2 * NB_BANDS],
        }
    }

    /// Drops all inter-frame state.
    pub fn reset(&mut self) {
        self.hist.fill(0.0);
        self.preemph_mem = [0.0; 2];
        self.old_band_e.fill(0.0);
        self.rng = 0;
        self.started = false;
        self.consec_transient = 0;
    }

    /// The range coder state after the last frame, the decoder's `rng`.
    pub fn rng(&self) -> u32 {
        self.rng
    }

    /// Encodes one CELT frame of `frame_size` samples per channel (at 48 kHz;
    /// `frame_size/upsample` samples are read from `pcm`) into `enc`.
    ///
    /// `end` bounds the coded bands, which is how the caller limits the coded
    /// bandwidth. The frame's byte budget is `enc.storage()`.
    pub fn encode(
        &mut self,
        pcm: &[f32],
        frame_size: usize,
        end: usize,
        enc: &mut RangeEncoder,
    ) -> Result<()> {
        let lm = match frame_size {
            120 => 0,
            240 => 1,
            480 => 2,
            960 => 3,
            _ => {
                return Err(Error::corrupt(format!(
                    "celt: frame size {frame_size} is not 120, 240, 480 or 960"
                )));
            }
        };
        let m = 1usize << lm;
        let n = m * SHORT_MDCT;
        let c = self.channels;
        let start = 0usize;
        let end = end.min(NB_BANDS);
        let total_bits = (enc.storage() * 8) as i32;
        let in_n = n / self.upsample;
        if pcm.len() < in_n * c {
            return Err(Error::corrupt(format!(
                "celt encode: {} samples for a {in_n}-sample {c}-channel frame",
                pcm.len()
            )));
        }

        self.preemphasis(pcm, n, c);

        // A frame the decoder would read as silence anyway: say so in one bit.
        let peak = (0..c)
            .flat_map(|ch| {
                let base = ch * (n + OVERLAP) + OVERLAP;
                self.work[base..base + n].iter()
            })
            .fold(0.0f32, |a, v| a.max(v.abs()));
        let silence = peak < 1e-2 || total_bits <= 1;
        if total_bits > 1 {
            enc.enc_bit_logp(silence, 15);
        }
        if silence {
            self.old_band_e.fill(-28.0);
            self.rng = enc.range();
            self.started = true;
            return Ok(());
        }

        // The post-filter is not used (its bit still has to be written).
        // corner-cut: no pitch pre-filter, so tonal speech loses the comb gain
        // libopus gets below ~40 kbps; ceiling is one bit per frame, and the
        // upgrade is a pitch search feeding the same three coded parameters.
        if start == 0 && enc.tell() as i32 + 16 <= total_bits {
            enc.enc_bit_logp(false, 1);
        }

        let bits_per_sample = total_bits as f32 / (n * c) as f32;
        let mut transient = lm > 0 && self.transient_analysis(n, c, bits_per_sample);
        let tell = enc.tell() as i32;
        if lm > 0 && tell + 3 <= total_bits {
            enc.enc_bit_logp(transient, 3);
        } else {
            transient = false;
        }
        self.consec_transient = if transient {
            self.consec_transient + 1
        } else {
            0
        };
        let short_blocks = if transient { m } else { 0 };
        let blocks = if transient { m } else { 1 };

        self.compute_mdcts(short_blocks, lm, n, c);
        self.compute_band_energies(lm, n, c, end);

        // --- coarse energy, intra or inter, whichever costs fewer bits -----
        let intra = self.quant_coarse_energy(enc, start, end, c, lm, total_bits);

        // --- time/frequency resolution ------------------------------------
        self.tf_analysis(enc, start, end, transient, lm, n, c, blocks, total_bits);

        // --- spreading ----------------------------------------------------
        let spread = self.spreading_decision(start, end, lm, c);
        if enc.tell() as i32 + 4 <= total_bits {
            enc.enc_icdf(spread, &SPREAD_ICDF, 5);
        }

        for i in 0..NB_BANDS {
            let bw = (E_BANDS[i + 1] - E_BANDS[i]) << lm;
            self.caps[i] =
                ((CACHE_CAPS[NB_BANDS * (2 * lm + c - 1) + i] as i32 + 64) * c as i32 * bw as i32)
                    >> 2;
        }

        // --- dynamic allocation -------------------------------------------
        let mut total_bits_frac = total_bits << BITRES;
        self.dynalloc_analysis(start, end, c, lm, total_bits, transient);
        let mut dynalloc_logp = 6u32;
        for i in start..end {
            let width = ((c * (E_BANDS[i + 1] - E_BANDS[i])) << lm) as i32;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0;
            let mut want = self.offsets[i];
            while enc.tell_frac() as i32 + ((loop_logp as i32) << BITRES) < total_bits_frac
                && boost < self.caps[i]
            {
                let flag = want >= quanta;
                enc.enc_bit_logp(flag, loop_logp);
                if !flag {
                    break;
                }
                boost += quanta;
                want -= quanta;
                total_bits_frac -= quanta;
                loop_logp = 1;
            }
            self.offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = dynalloc_logp.saturating_sub(1).max(2);
            }
        }

        // --- allocation trim ----------------------------------------------
        let alloc_trim = self.trim_analysis(start, end, c, lm, intra);
        if enc.tell_frac() as i32 + (6 << BITRES) <= total_bits_frac {
            enc.enc_icdf(alloc_trim as usize, &TRIM_ICDF, 7);
        }

        let mut bits = ((enc.storage() as i32 * 8) << BITRES) - enc.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;

        // --- stereo decisions, then the shared allocation ------------------
        let (intensity_want, dual_want) = self.stereo_analysis(start, end, c, lm, bits);
        let mut intensity = 0usize;
        let mut dual_stereo = false;
        let mut balance = 0i32;
        let coded_bands = {
            let mut coder = EncAlloc {
                enc,
                intensity: intensity_want,
                dual: dual_want,
            };
            let mut arrays = AllocArrays {
                caps: &self.caps,
                offsets: &self.offsets,
                pulses: &mut self.pulses,
                fine_quant: &mut self.fine_quant,
                fine_priority: &mut self.fine_priority,
            };
            celt::compute_allocation(
                &mut coder,
                &mut arrays,
                start,
                end,
                alloc_trim,
                &mut intensity,
                &mut dual_stereo,
                bits,
                &mut balance,
                c,
                lm,
            )
        };

        self.quant_fine_energy(enc, start, end, c);

        let band_budget = (enc.storage() as i32 * (8 << BITRES)) - anti_collapse_rsv;
        self.quant_all_bands(
            enc,
            start,
            end,
            short_blocks,
            spread,
            dual_stereo,
            intensity,
            band_budget,
            balance,
            lm,
            coded_bands,
            c,
            n,
        );

        if anti_collapse_rsv > 0 {
            // The bands that collapsed are exactly the ones the decoder will
            // fill; signalling it costs nothing beyond the reserved bit.
            enc.enc_bits(1, 1);
        }

        let bits_left = enc.storage() as i32 * 8 - enc.tell() as i32;
        self.quant_energy_finalise(enc, start, end, bits_left, c);

        for ch in 0..2 {
            for i in (0..start).chain(end..NB_BANDS) {
                self.old_band_e[ch * NB_BANDS + i] = 0.0;
            }
        }
        if c == 1 {
            for i in 0..NB_BANDS {
                self.old_band_e[NB_BANDS + i] = self.old_band_e[i];
            }
        }
        self.rng = enc.range();
        self.started = true;
        Ok(())
    }

    // -- analysis ----------------------------------------------------------

    /// `x[k] - 0.85 x[k-1]`, the exact inverse of the decoder's de-emphasis,
    /// into `work` behind one overlap of history.
    fn preemphasis(&mut self, pcm: &[f32], n: usize, c: usize) {
        let up = self.upsample;
        for ch in 0..c {
            let base = ch * (n + OVERLAP);
            let (hist, work) = (&mut self.hist, &mut self.work);
            work[base..base + OVERLAP].copy_from_slice(&hist[ch * OVERLAP..(ch + 1) * OVERLAP]);
            let mut mem = self.preemph_mem[ch];
            if up == 1 {
                for j in 0..n {
                    let s = pcm[j * c + ch] * SIG_SCALE;
                    work[base + OVERLAP + j] = s - PREEMPH * mem;
                    mem = s;
                }
            } else {
                // Zero-stuffing to 48 kHz: the images above the input's Nyquist
                // are never coded (the caller caps `end`), and the decoder's
                // decimation drops them.
                for j in 0..n {
                    let s = if j.is_multiple_of(up) {
                        pcm[(j / up) * c + ch] * SIG_SCALE * up as f32
                    } else {
                        0.0
                    };
                    work[base + OVERLAP + j] = s - PREEMPH * mem;
                    mem = s;
                }
            }
            self.preemph_mem[ch] = mem;
            hist[ch * OVERLAP..(ch + 1) * OVERLAP]
                .copy_from_slice(&work[base + n..base + n + OVERLAP]);
        }
    }

    /// Short blocks when a sub-block's energy jumps far above the ones before
    /// it — the pre-echo the long MDCT would otherwise smear over 20 ms.
    fn transient_analysis(&self, n: usize, c: usize, bits_per_sample: f32) -> bool {
        const SUBS: usize = 16;
        let sub = n / SUBS;
        if sub < 8 {
            return false;
        }
        // The overlap that precedes the frame is the attack's "before": an
        // onset in the first sub-block is exactly the one a long MDCT smears
        // backwards past the frame boundary.
        let mut e = [0.0f32; SUBS + 1];
        for ch in 0..c {
            let base = ch * (n + OVERLAP);
            let head = &self.work[base + OVERLAP - sub..base + OVERLAP];
            e[0] += head.iter().map(|v| v * v).sum::<f32>() / sub as f32;
            for b in 0..SUBS {
                let seg = &self.work[base + OVERLAP + b * sub..base + OVERLAP + (b + 1) * sub];
                e[b + 1] += seg.iter().map(|v| v * v).sum::<f32>() / sub as f32;
            }
        }
        // Each sub-block against the mean of everything before it. An
        // exponential mean with a fast constant tracks the attack itself and
        // hides it; the running mean does not.
        let mut sum = e[0].max(1e-4);
        let mut count = 1.0f32;
        let mut worst = 0.0f32;
        for &v in &e[1..] {
            worst = worst.max(v / (sum / count).max(1e-4));
            sum += v;
            count += 1.0;
        }
        // How large a jump is worth splitting the frame for depends on what
        // the split costs: short blocks buy pre-echo control and pay coding
        // gain, so at 2.5 bits a sample the encoder takes almost any attack
        // and at half a bit it takes only the ones it cannot hide.
        // (Measured on speech: at 256 kbps stereo the sensitive threshold is
        // worth 11 points of opus_compare, at 64 kbps it costs 50.)
        let threshold = if bits_per_sample < 0.9 {
            // Short blocks buy pre-echo control and pay coding gain, and below
            // a bit a sample this encoder loses more than it saves: measured on
            // his library, 2ch 64 kbps scored -619 with this gate and -891
            // without, and 2ch 256 kbps (where the gate never fires) gained 11
            // points from the sensitive threshold below.
            f32::INFINITY
        } else {
            (24.0 - 8.0 * bits_per_sample).max(3.0)
        };
        worst > threshold && e.iter().fold(0.0f32, |a, &v| a.max(v)) > 1.0
    }

    /// Windowed forward MDCT of the whole frame, one long block or `M` short
    /// ones interleaved in frequency exactly as the decoder reads them.
    fn compute_mdcts(&mut self, short_blocks: usize, lm: usize, n: usize, c: usize) {
        let (n2, blocks, plan_lm) = if short_blocks != 0 {
            (SHORT_MDCT, short_blocks, 0)
        } else {
            (n, 1, lm)
        };
        let plan = &mut self.plans[plan_lm];
        let (work, window, freq) = (&self.work, &self.window, &mut self.freq);
        for ch in 0..c {
            let base = ch * (n + OVERLAP);
            for b in 0..blocks {
                let inp = &work[base + b * n2..base + b * n2 + n2 + OVERLAP];
                plan.forward(inp, window, &mut freq[ch * n + b..], blocks);
            }
        }
    }

    /// Per-band energy in log2 with the mean removed (what gets coded), and the
    /// unit-norm band shapes the PVQ will quantise.
    fn compute_band_energies(&mut self, lm: usize, n: usize, c: usize, end: usize) {
        let m = 1usize << lm;
        for ch in 0..c {
            for i in 0..NB_BANDS {
                let lo = ch * n + m * E_BANDS[i];
                let hi = ch * n + m * E_BANDS[i + 1];
                let e: f32 = self.freq[lo..hi].iter().map(|v| v * v).sum::<f32>() + 1e-27;
                let amp = e.sqrt();
                self.band_e[ch * NB_BANDS + i] = amp;
                self.band_log_e[ch * NB_BANDS + i] = if i < end {
                    amp.log2() - E_MEANS[i]
                } else {
                    -28.0
                };
                let g = 1.0 / amp;
                for j in lo..hi {
                    self.x[j] = self.freq[j] * g;
                }
            }
        }
    }

    // -- energy quantisation ------------------------------------------------

    /// Codes the coarse energy both ways and keeps the cheaper: the intra model
    /// wins on transients and after a reset, the predicted one on steady music.
    /// Returns the flag that was coded.
    fn quant_coarse_energy(
        &mut self,
        enc: &mut RangeEncoder,
        start: usize,
        end: usize,
        c: usize,
        lm: usize,
        total_bits: i32,
    ) -> bool {
        let tell = enc.tell() as i32;
        if tell + 3 > total_bits {
            // No room for the flag: the decoder assumes inter.
            let mut old = core::mem::take(&mut self.old_band_e);
            let mut err = core::mem::take(&mut self.err);
            coarse_energy(
                enc,
                &mut old,
                &self.band_log_e,
                &mut err,
                start,
                end,
                false,
                c,
                lm,
                total_bits,
            );
            self.old_band_e = old;
            self.err = err;
            return false;
        }

        let mut inter_enc = enc.clone();
        self.scratch_old.copy_from_slice(&self.old_band_e);
        inter_enc.enc_bit_logp(false, 3);
        coarse_energy(
            &mut inter_enc,
            &mut self.scratch_old,
            &self.band_log_e,
            &mut self.scratch_err,
            start,
            end,
            false,
            c,
            lm,
            total_bits,
        );
        let inter_bits = inter_enc.tell();

        let mut intra_enc = enc.clone();
        let mut intra_old = self.old_band_e.clone();
        let mut intra_err = vec![0.0f32; 2 * NB_BANDS];
        intra_enc.enc_bit_logp(true, 3);
        coarse_energy(
            &mut intra_enc,
            &mut intra_old,
            &self.band_log_e,
            &mut intra_err,
            start,
            end,
            true,
            c,
            lm,
            total_bits,
        );
        let intra_bits = intra_enc.tell();

        // The prediction is worth keeping when it is not much more expensive:
        // an intra frame costs the next frame's prediction nothing, but a
        // frame that predicts badly costs bits everywhere else in this one.
        if !self.started || intra_bits < inter_bits {
            *enc = intra_enc;
            self.old_band_e.copy_from_slice(&intra_old);
            self.err.copy_from_slice(&intra_err);
            true
        } else {
            *enc = inter_enc;
            self.old_band_e.copy_from_slice(&self.scratch_old);
            self.err.copy_from_slice(&self.scratch_err);
            false
        }
    }

    fn quant_fine_energy(&mut self, enc: &mut RangeEncoder, start: usize, end: usize, c: usize) {
        for i in start..end {
            if self.fine_quant[i] <= 0 {
                continue;
            }
            let frac = (1i32 << self.fine_quant[i]) as f32;
            for ch in 0..c {
                let k = i + ch * NB_BANDS;
                let q2 = (((self.err[k] + 0.5) * frac).floor() as i32).clamp(0, frac as i32 - 1);
                enc.enc_bits(q2 as u32, self.fine_quant[i] as u32);
                let offset = (q2 as f32 + 0.5) / frac - 0.5;
                self.old_band_e[k] += offset;
                self.err[k] -= offset;
            }
        }
    }

    fn quant_energy_finalise(
        &mut self,
        enc: &mut RangeEncoder,
        start: usize,
        end: usize,
        mut bits_left: i32,
        c: usize,
    ) {
        for prio in 0..2 {
            for i in start..end {
                if bits_left < c as i32 {
                    break;
                }
                if self.fine_quant[i] >= MAX_FINE_BITS || self.fine_priority[i] != prio {
                    continue;
                }
                for ch in 0..c {
                    let k = i + ch * NB_BANDS;
                    let q2 = u32::from(self.err[k] > 0.0);
                    enc.enc_bits(q2, 1);
                    let offset = (q2 as f32 - 0.5) / (1i32 << (self.fine_quant[i] + 1)) as f32;
                    self.old_band_e[k] += offset;
                    self.err[k] -= offset;
                    bits_left -= 1;
                }
            }
        }
    }

    // -- per-frame decisions ------------------------------------------------

    /// Picks the per-band time/frequency resolution and codes it.
    ///
    /// The candidate resolutions for a band are the two entries of
    /// [`TF_SELECT`] its toggle bit can reach; the metric is the L1 norm of the
    /// band after the Haar passes that resolution implies, normalised by its L2
    /// norm — the sparser the band, the fewer pulses buy the same shape. The
    /// toggle chain is then solved by a two-state Viterbi so that a resolution
    /// change is only taken when it beats what coding the change costs.
    #[allow(clippy::too_many_arguments)]
    fn tf_analysis(
        &mut self,
        enc: &mut RangeEncoder,
        start: usize,
        end: usize,
        transient: bool,
        lm: usize,
        n: usize,
        c: usize,
        blocks: usize,
        total_bits: i32,
    ) {
        let m = 1usize << lm;
        let t = usize::from(transient);
        // metric[band][option] in bits, option 0/1 = the toggle bit's value.
        let mut metric = [[0.0f32; 2]; NB_BANDS];
        let mut metric_s1 = [[0.0f32; 2]; NB_BANDS];
        for i in start..end {
            for (sel, dst) in [(0usize, &mut metric), (1, &mut metric_s1)] {
                for ch in 0..2usize {
                    let r = TF_SELECT[lm][4 * t + 2 * sel + ch];
                    dst[i][ch] = self.tf_cost(i, m, n, c, blocks, r);
                }
            }
        }

        let mut best: Option<(f32, usize, [i32; NB_BANDS], bool)> = None;
        for sel in 0..2usize {
            let met = if sel == 0 { &metric } else { &metric_s1 };
            let (cost, res, changed) = tf_viterbi(met, start, end, transient);
            let usable = sel == 0
                || (lm > 0
                    && TF_SELECT[lm][4 * t + usize::from(changed)]
                        != TF_SELECT[lm][4 * t + 2 + usize::from(changed)]);
            if usable && best.as_ref().is_none_or(|b| cost < b.0) {
                best = Some((cost, sel, res, changed));
            }
        }
        let (_, mut tf_select, res, _) = best.expect("tf: at least tf_select 0 is always usable");

        // Code the toggle chain, mirroring the decoder's budget checks exactly.
        let mut budget = total_bits;
        let mut tell = enc.tell() as i32;
        let mut logp: u32 = if transient { 2 } else { 4 };
        let tf_select_rsv = lm > 0 && tell + (logp as i32) < budget;
        budget -= i32::from(tf_select_rsv);
        let mut curr = 0i32;
        let mut changed = false;
        for (i, &want) in res.iter().enumerate().take(end).skip(start) {
            if tell + logp as i32 <= budget {
                let bit = want != curr;
                enc.enc_bit_logp(bit, logp);
                curr ^= i32::from(bit);
                tell = enc.tell() as i32;
                changed |= curr != 0;
            }
            self.tf_res[i] = curr;
            logp = if transient { 4 } else { 5 };
        }
        let ch = usize::from(changed);
        if tf_select_rsv && TF_SELECT[lm][4 * t + ch] != TF_SELECT[lm][4 * t + 2 + ch] {
            enc.enc_bit_logp(tf_select == 1, 1);
        } else {
            tf_select = 0;
        }
        for i in start..end {
            self.tf_res[i] = TF_SELECT[lm][4 * t + 2 * tf_select + self.tf_res[i] as usize];
        }
    }

    /// The L1/L2 sparsity of band `i` after the Haar passes resolution `r`
    /// implies, in bits: `N * log2(L1/L2)` is zero for a single spike and
    /// `N/2 * log2(N)` for a flat band, which is the shape of the PVQ's cost.
    #[allow(clippy::too_many_arguments)]
    fn tf_cost(&mut self, i: usize, m: usize, n: usize, c: usize, blocks: usize, r: i32) -> f32 {
        let n_band = m * (E_BANDS[i + 1] - E_BANDS[i]);
        let mut total = 0.0;
        // Copy the band (per channel) and apply the Haar passes.
        for ch in 0..c {
            let base = ch * n + m * E_BANDS[i];
            let src = &self.x[base..base + n_band];
            let tmp = &mut self.tf_tmp[..n_band];
            tmp.copy_from_slice(src);
            let mut nb = n_band / blocks;
            let mut b = blocks;
            if r > 0 {
                for k in 0..r as usize {
                    if (n_band >> k) < 2 {
                        break;
                    }
                    haar1(tmp, n_band >> k, 1 << k);
                }
            } else {
                let mut left = -r;
                while left > 0 && nb.is_multiple_of(2) {
                    haar1(tmp, nb, b);
                    b <<= 1;
                    nb >>= 1;
                    left -= 1;
                }
            }
            let l1: f32 = tmp.iter().map(|v| v.abs()).sum();
            let l2: f32 = tmp.iter().map(|v| v * v).sum::<f32>().sqrt() + 1e-15;
            total += n_band as f32 * (l1 / l2).max(1.0).log2();
        }
        total
    }

    /// How hard to rotate the PVQ codewords: tonal bands keep their peaks,
    /// noisy ones get spread so that a handful of pulses does not sound like a
    /// handful of pulses.
    fn spreading_decision(&self, start: usize, end: usize, lm: usize, c: usize) -> usize {
        let m = 1usize << lm;
        let n = m * SHORT_MDCT;
        let mut sum = 0.0f32;
        let mut weight = 0.0f32;
        for ch in 0..c {
            for i in start..end {
                let n_band = m * (E_BANDS[i + 1] - E_BANDS[i]);
                if n_band < 4 {
                    continue;
                }
                let base = ch * n + m * E_BANDS[i];
                let seg = &self.x[base..base + n_band];
                let l1: f32 = seg.iter().map(|v| v.abs()).sum();
                let l2: f32 = seg.iter().map(|v| v * v).sum::<f32>().sqrt() + 1e-15;
                // 1 for a flat band, 1/sqrt(N) for a single spike.
                sum += (l1 / (l2 * (n_band as f32).sqrt())) * n_band as f32;
                weight += n_band as f32;
            }
        }
        if weight <= 0.0 {
            return SPREAD_NORMAL;
        }
        let flat = sum / weight;
        if flat > 0.72 {
            SPREAD_AGGRESSIVE
        } else if flat > 0.45 {
            SPREAD_NORMAL
        } else if flat > 0.28 {
            1 // light
        } else {
            SPREAD_NONE
        }
    }

    /// Extra bits for bands that stand above what their neighbours predict.
    ///
    /// A spreading-masking follower gives each band the loudest of its
    /// neighbours minus a per-band decay; a band well above its own follower is
    /// a spectral peak that the static allocation would under-serve, and it is
    /// exactly where a shortage of pulses is audible. The total boost is capped
    /// at a share of the frame so that dynalloc cannot starve everything else.
    fn dynalloc_analysis(
        &mut self,
        start: usize,
        end: usize,
        c: usize,
        lm: usize,
        total_bits: i32,
        transient: bool,
    ) {
        self.offsets.fill(0);
        if end <= start + 2 {
            return;
        }
        let mut e = [0.0f32; NB_BANDS];
        for (i, slot) in e.iter_mut().enumerate().take(end).skip(start) {
            *slot = if c == 2 {
                0.5 * (self.band_log_e[i] + self.band_log_e[NB_BANDS + i])
            } else {
                self.band_log_e[i]
            };
        }
        // Two leaky maxima, one from each side, of the *other* bands.
        const DECAY: f32 = 1.5; // log2 units per band, ~9 dB
        let mut up = [-32.0f32; NB_BANDS];
        let mut down = [-32.0f32; NB_BANDS];
        for i in start + 1..end {
            up[i] = (up[i - 1] - DECAY).max(e[i - 1] - DECAY);
        }
        for i in (start..end - 1).rev() {
            down[i] = (down[i + 1] - DECAY).max(e[i + 1] - DECAY);
        }
        let mut budget = total_bits / 12; // never more than ~8% of the frame
        for i in start..end {
            let follower = up[i].max(down[i]);
            let excess = e[i] - follower;
            if excess <= 1.0 {
                continue;
            }
            let width = ((c * (E_BANDS[i + 1] - E_BANDS[i])) << lm) as i32;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            // One quantum per 6 dB above the follower, three at most; a
            // transient's low bands get one extra (that is where the attack is).
            let mut units = ((excess - 0.5) as i32).clamp(0, 3);
            if transient && i < start + 4 {
                units += 1;
            }
            let mut boost = units * quanta;
            boost = boost.min(self.caps[i]).min(budget << BITRES);
            if boost > 0 {
                budget -= boost >> BITRES;
                self.offsets[i] = boost;
            }
            if budget <= 0 {
                break;
            }
        }
    }

    /// Tilts the static allocation with the measured spectral tilt: a bright
    /// signal wants its bits higher up than the table's default assumes.
    fn trim_analysis(&self, start: usize, end: usize, c: usize, lm: usize, intra: bool) -> i32 {
        if end <= start + 2 {
            return 5;
        }
        let band = |i: usize| {
            if c == 2 {
                0.5 * (self.band_log_e[i] + self.band_log_e[NB_BANDS + i])
            } else {
                self.band_log_e[i]
            }
        };
        // Least-squares slope of the band energies against the band index,
        // in log2 units per band.
        let cnt = (end - start) as f32;
        let mean_i = (start as f32 + end as f32 - 1.0) / 2.0;
        let mut mean_e = 0.0;
        for i in start..end {
            mean_e += band(i);
        }
        mean_e /= cnt;
        let (mut num, mut den) = (0.0f32, 0.0f32);
        for i in start..end {
            let di = i as f32 - mean_i;
            num += di * (band(i) - mean_e);
            den += di * di;
        }
        let slope = num / den.max(1e-6);
        // The allocation's neutral point is 5 + LM; a positive slope (energy
        // rising with frequency) asks for bits to move up, which is trim down.
        let mut trim = 5.0 + lm as f32 - 6.0 * slope;
        if intra {
            trim -= 1.0;
        }
        (trim.round() as i32).clamp(0, 10)
    }

    /// Where intensity stereo starts, and whether to code the channels apart.
    ///
    /// Both are measured, not assumed: the intensity threshold is the lowest
    /// band above which every band's side energy is a negligible share of its
    /// total (so folding the side away is inaudible), pulled down when the
    /// frame is too small to afford full stereo; dual stereo is only worth its
    /// bit when mid/side would not concentrate anything, i.e. when the channels
    /// are nearly uncorrelated.
    fn stereo_analysis(
        &self,
        start: usize,
        end: usize,
        c: usize,
        lm: usize,
        bits: i32,
    ) -> (usize, bool) {
        if c != 2 {
            return (end, false);
        }
        let m = 1usize << lm;
        let n = m * SHORT_MDCT;
        let mut lowest = end;
        let mut e_mid_total = 0.0f64;
        let mut e_side_total = 0.0f64;
        for i in (start..end).rev() {
            let lo = m * E_BANDS[i];
            let hi = m * E_BANDS[i + 1];
            let (mut em, mut es) = (0.0f32, 0.0f32);
            for j in lo..hi {
                // Band shapes are unit norm, so weight by the coded energy.
                let l = self.x[j] * self.band_e[i];
                let r = self.x[n + j] * self.band_e[NB_BANDS + i];
                let mid = 0.5 * (l + r);
                let side = 0.5 * (r - l);
                em += mid * mid;
                es += side * side;
            }
            e_mid_total += em as f64;
            e_side_total += es as f64;
            if es <= 0.02 * (em + es) {
                lowest = i;
            } else {
                break;
            }
        }
        // Bits per coefficient across the coded band; below ~0.8 the side is
        // not affordable however wide the image is.
        let coeffs = (m * E_BANDS[end].saturating_sub(E_BANDS[start])) as i32;
        let bps = if coeffs > 0 {
            (bits >> BITRES) as f32 / coeffs as f32
        } else {
            0.0
        };
        let mut intensity = lowest;
        if bps < 0.8 {
            let cut = start + ((end - start) as f32 * (0.35 + 0.8 * bps)) as usize;
            intensity = intensity.min(cut.max(start + 1));
        }
        let dual = e_side_total > 0.8 * e_mid_total && bps > 1.5;
        (intensity.min(end), dual)
    }

    // -- band quantisation --------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn quant_all_bands(
        &mut self,
        enc: &mut RangeEncoder,
        start: usize,
        end: usize,
        short_blocks: usize,
        spread: usize,
        mut dual_stereo: bool,
        intensity: usize,
        total_bits: i32,
        mut balance: i32,
        lm: usize,
        coded_bands: usize,
        c: usize,
        n: usize,
    ) {
        let m = 1usize << lm;
        let blocks = if short_blocks != 0 { m } else { 1 };
        let norm_len = m * E_BANDS[NB_BANDS];
        let mut lowband_offset = 0usize;
        let mut update_lowband = true;
        self.collapse_masks.fill(0);

        for i in start..end {
            let n_band = m * E_BANDS[i + 1] - m * E_BANDS[i];
            let tell = enc.tell_frac() as i32;
            if i != start {
                balance -= tell;
            }
            let mut remaining_bits = total_bits - tell - 1;
            let b = if i < coded_bands {
                let curr_balance = balance / 3.min(coded_bands - i) as i32;
                0.max(16383.min((remaining_bits + 1).min(self.pulses[i] + curr_balance)))
            } else {
                0
            };

            if m * E_BANDS[i] >= m * E_BANDS[start] + n_band
                && (update_lowband || lowband_offset == 0)
            {
                lowband_offset = i;
            }

            let tf_change = self.tf_res[i];
            let (mut x_cm, mut y_cm);
            let mut effective_lowband: Option<usize> = None;
            if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || blocks > 1 || tf_change < 0) {
                let low =
                    (m * E_BANDS[start]).max((m * E_BANDS[lowband_offset]).saturating_sub(n_band));
                effective_lowband = Some(low);
                let mut fold_start = lowband_offset;
                loop {
                    fold_start -= 1;
                    if m * E_BANDS[fold_start] <= low {
                        break;
                    }
                }
                let mut fold_end = lowband_offset - 1;
                loop {
                    fold_end += 1;
                    if m * E_BANDS[fold_end] >= low + n_band {
                        break;
                    }
                }
                x_cm = 0;
                y_cm = 0;
                let mut fi = fold_start;
                loop {
                    x_cm |= self.collapse_masks[fi * c] as u32;
                    y_cm |= self.collapse_masks[fi * c + c - 1] as u32;
                    fi += 1;
                    if fi >= fold_end {
                        break;
                    }
                }
            } else {
                x_cm = (1u32 << blocks) - 1;
                y_cm = x_cm;
            }

            if dual_stereo && i == intensity {
                dual_stereo = false;
                for j in m * E_BANDS[start]..m * E_BANDS[i] {
                    self.norm[j] = 0.5 * (self.norm[j] + self.norm[norm_len + j]);
                }
            }

            let x_off = m * E_BANDS[i];
            let base = BandArgs {
                i,
                x: x_off,
                y: None,
                n: n_band,
                b,
                spread,
                blocks,
                intensity,
                tf_change,
                lowband: Lowband::None,
                level: 0,
                lm: lm as i32,
                lowband_out: Some(x_off),
                gain: 1.0,
                fill: 0,
            };
            if dual_stereo {
                x_cm = self.quant_band(
                    enc,
                    BandArgs {
                        b: b / 2,
                        lowband: effective_lowband.map_or(Lowband::None, Lowband::Norm),
                        fill: x_cm,
                        ..base
                    },
                    &mut remaining_bits,
                );
                y_cm = self.quant_band(
                    enc,
                    BandArgs {
                        x: n + x_off,
                        b: b / 2,
                        lowband: effective_lowband
                            .map_or(Lowband::None, |l| Lowband::Norm(norm_len + l)),
                        lowband_out: Some(norm_len + x_off),
                        fill: y_cm,
                        ..base
                    },
                    &mut remaining_bits,
                );
            } else {
                x_cm = self.quant_band(
                    enc,
                    BandArgs {
                        y: if c == 2 { Some(n + x_off) } else { None },
                        lowband: effective_lowband.map_or(Lowband::None, Lowband::Norm),
                        fill: x_cm | y_cm,
                        ..base
                    },
                    &mut remaining_bits,
                );
                y_cm = x_cm;
            }
            self.collapse_masks[i * c] = x_cm as u8;
            self.collapse_masks[i * c + c - 1] = y_cm as u8;
            balance += self.pulses[i] + tell;
            update_lowband = b > (n_band as i32) << BITRES;
        }
    }

    /// Encodes one band, recursing for splits and for stereo — the exact
    /// counterpart of `CeltDecoder::quant_band`, decision for decision.
    fn quant_band(&mut self, enc: &mut RangeEncoder, a: BandArgs, remaining_bits: &mut i32) -> u32 {
        let BandArgs {
            i,
            x,
            mut y,
            mut n,
            mut b,
            spread,
            mut blocks,
            intensity,
            mut tf_change,
            mut lowband,
            level,
            mut lm,
            lowband_out,
            gain,
            mut fill,
        } = a;
        let n0 = n;
        let mut n_b = n;
        let stereo = y.is_some();
        let mut split = stereo;
        let mut inv = false;
        let mut cm: u32 = 0;
        let mut time_divide = 0;
        let mut recombine = 0usize;
        let long_blocks = blocks == 1;

        n_b /= blocks;
        let mut n_b0 = n_b;
        let mut b0 = blocks;

        if n == 1 {
            for offs in [Some(x), y].into_iter().flatten() {
                let mut sign = self.x[offs] < 0.0;
                if *remaining_bits >= 1 << BITRES {
                    enc.enc_bits(u32::from(sign), 1);
                    *remaining_bits -= 1 << BITRES;
                } else {
                    sign = false;
                }
                self.x[offs] = if sign { -1.0 } else { 1.0 };
            }
            if let Some(o) = lowband_out {
                self.norm[o] = self.x[x];
            }
            return 1;
        }

        if !stereo && level == 0 {
            if tf_change > 0 {
                recombine = tf_change as usize;
            }
            if lowband != Lowband::None
                && (recombine > 0 || (n_b & 1) == 0 && tf_change < 0 || b0 > 1)
            {
                for j in 0..n {
                    self.lowband_scratch[j] = self.lowband(lowband, j);
                }
                lowband = Lowband::Scratch(0);
            }
            for k in 0..recombine {
                const BIT_INTERLEAVE: [u8; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
                haar1(&mut self.x[x..], n >> k, 1 << k);
                if let Some(l) = self.lowband_mut(lowband) {
                    haar1(l, n >> k, 1 << k);
                }
                fill = BIT_INTERLEAVE[(fill & 0xF) as usize] as u32
                    | (BIT_INTERLEAVE[(fill >> 4) as usize] as u32) << 2;
            }
            blocks >>= recombine;
            n_b <<= recombine;
            while (n_b & 1) == 0 && tf_change < 0 {
                haar1(&mut self.x[x..], n_b, blocks);
                if let Some(l) = self.lowband_mut(lowband) {
                    haar1(l, n_b, blocks);
                }
                fill |= fill << blocks;
                blocks <<= 1;
                n_b >>= 1;
                time_divide += 1;
                tf_change += 1;
            }
            b0 = blocks;
            n_b0 = n_b;
            if b0 > 1 {
                deinterleave_hadamard(
                    &mut self.x[x..x + n],
                    &mut self.hadamard_tmp,
                    n_b >> recombine,
                    b0 << recombine,
                    long_blocks,
                );
                if lowband != Lowband::None {
                    let mut tmp = core::mem::take(&mut self.hadamard_tmp);
                    if let Some(l) = self.lowband_mut(lowband) {
                        deinterleave_hadamard(
                            &mut l[..n],
                            tmp.as_mut_slice(),
                            n_b >> recombine,
                            b0 << recombine,
                            long_blocks,
                        );
                    }
                    self.hadamard_tmp = tmp;
                }
            }
        }

        let cache_start = cache_index(i, lm);
        let cache0 = celt::CACHE_BITS[cache_start] as usize;
        if !stereo && lm != -1 && b > celt::CACHE_BITS[cache_start + cache0] as i32 + 12 && n > 2 {
            n >>= 1;
            y = Some(x + n);
            split = true;
            lm -= 1;
            if blocks == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            blocks = (blocks + 1) >> 1;
        }

        let mut mid = 0.0f32;
        if split {
            let yy = y.expect("a split band always has a second half");
            let pulse_cap = LOG_N[i] + lm * (1 << BITRES);
            let offset = (pulse_cap >> 1)
                - if stereo && n == 2 {
                    QTHETA_OFFSET_TWOPHASE
                } else {
                    QTHETA_OFFSET
                };
            let mut qn = compute_qn(n as i32, b, offset, pulse_cap, stereo);
            if stereo && i >= intensity {
                qn = 1;
            }
            let tell = enc.tell_frac() as i32;

            // The angle between the two halves, measured on the signal.
            let (e_mid, e_side) = if stereo {
                let mut em = 1e-15f32;
                let mut es = 1e-15f32;
                for j in 0..n {
                    let m0 = 0.5 * (self.x[x + j] + self.x[yy + j]);
                    let s0 = 0.5 * (self.x[yy + j] - self.x[x + j]);
                    em += m0 * m0;
                    es += s0 * s0;
                }
                (em, es)
            } else {
                let mut em = 1e-15f32;
                let mut es = 1e-15f32;
                for j in 0..n {
                    em += self.x[x + j] * self.x[x + j];
                    es += self.x[yy + j] * self.x[yy + j];
                }
                (em, es)
            };
            let theta01 = e_side.sqrt().atan2(e_mid.sqrt()) * core::f32::consts::FRAC_2_PI;
            let itheta_full = (theta01 * 16384.0).round().clamp(0.0, 16384.0) as i32;

            let mut itheta: i32 = 0;
            if qn != 1 {
                let q = (((itheta_full as i64 * qn as i64 + 8192) >> 14) as i32).clamp(0, qn);
                if stereo && n > 2 {
                    // A step pdf: probability 3 up to itheta=qn/2, then 1.
                    let p0 = 3i32;
                    let x0 = qn / 2;
                    let ft = p0 * (x0 + 1) + x0;
                    let (fl, fh) = if q <= x0 {
                        (p0 * q, p0 * (q + 1))
                    } else {
                        ((q - 1 - x0) + (x0 + 1) * p0, (q - x0) + (x0 + 1) * p0)
                    };
                    enc.encode(fl as u32, fh as u32, ft as u32);
                } else if b0 > 1 || stereo {
                    enc.enc_uint(q as u32, (qn + 1) as u32);
                } else {
                    // Triangular pdf.
                    let ft = ((qn >> 1) + 1) * ((qn >> 1) + 1);
                    let (fl, fs) = if q < (qn >> 1) {
                        ((q * (q + 1)) >> 1, q + 1)
                    } else {
                        (ft - (((qn + 1 - q) * (qn + 2 - q)) >> 1), qn + 1 - q)
                    };
                    enc.encode(fl as u32, (fl + fs) as u32, ft as u32);
                }
                itheta = (q * 16384) / qn;
            } else if stereo {
                if b > 2 << BITRES && *remaining_bits > 2 << BITRES {
                    // Anti-phase channels: signal it rather than cancel them.
                    let mut dot = 0.0f32;
                    for j in 0..n {
                        dot += self.x[x + j] * self.x[yy + j];
                    }
                    inv = dot < 0.0;
                    enc.enc_bit_logp(inv, 2);
                }
                itheta = 0;
            }
            let qalloc = enc.tell_frac() as i32 - tell;
            b -= qalloc;

            let orig_fill = fill;
            let (imid, iside, mut delta);
            if itheta == 0 {
                imid = 32767;
                iside = 0;
                fill &= (1 << blocks) - 1;
                delta = -16384;
            } else if itheta == 16384 {
                imid = 0;
                iside = 32767;
                fill &= ((1u32 << blocks) - 1) << blocks;
                delta = 16384;
            } else {
                imid = celt::bitexact_cos(itheta as i16) as i32;
                iside = celt::bitexact_cos((16384 - itheta) as i16) as i32;
                delta = celt::frac_mul16(
                    ((n as i32 - 1) << 7) as i16,
                    celt::bitexact_log2tan(iside, imid) as i16,
                );
            }
            mid = imid as f32 / 32768.0;
            let side = iside as f32 / 32768.0;

            if stereo {
                // Left/right to mid/side, in place, before the two halves are
                // quantised independently.
                for j in 0..n {
                    let l = self.x[x + j];
                    let r = self.x[yy + j];
                    self.x[x + j] = 0.5 * (l + r);
                    self.x[yy + j] = 0.5 * (r - l);
                }
            }

            if n == 2 && stereo {
                let mut mbits = b;
                let mut sbits = 0;
                if itheta != 0 && itheta != 16384 {
                    sbits = 1 << BITRES;
                }
                mbits -= sbits;
                let swap = itheta > 8192;
                *remaining_bits -= qalloc + sbits;
                let (x2, y2) = if swap { (yy, x) } else { (x, yy) };
                // The second half is a quarter turn of the first, so only its
                // sense is coded: pick the one that matches the signal.
                let sign_bit = if sbits != 0 {
                    let want = -self.x[y2] * self.x[x2 + 1] + self.x[y2 + 1] * self.x[x2];
                    let s = u32::from(want < 0.0);
                    enc.enc_bits(s, 1);
                    s as i32
                } else {
                    0
                };
                let sign = 1 - 2 * sign_bit;
                cm = self.quant_band(
                    enc,
                    BandArgs {
                        x: x2,
                        y: None,
                        n,
                        b: mbits,
                        blocks,
                        tf_change,
                        lowband,
                        lm,
                        fill: orig_fill,
                        ..a
                    },
                    remaining_bits,
                );
                self.x[y2] = -(sign as f32) * self.x[x2 + 1];
                self.x[y2 + 1] = sign as f32 * self.x[x2];
                let x0 = self.x[x] * mid;
                let x1 = self.x[x + 1] * mid;
                let y0 = self.x[yy] * side;
                let y1 = self.x[yy + 1] * side;
                self.x[x] = x0 - y0;
                self.x[yy] = x0 + y0;
                self.x[x + 1] = x1 - y1;
                self.x[yy + 1] = x1 + y1;
            } else {
                if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                    if itheta > 8192 {
                        delta -= delta >> (4 - lm);
                    } else {
                        delta = 0.min(delta + ((n as i32) << BITRES >> (5 - lm)));
                    }
                }
                let mut mbits = 0.max(b.min((b - delta) / 2));
                let mut sbits = b - mbits;
                *remaining_bits -= qalloc;
                let next_lowband2 = if lowband != Lowband::None && !stereo {
                    lowband.offset(n)
                } else {
                    Lowband::None
                };
                let next_lowband_out1 = if stereo { lowband_out } else { None };
                let next_level = if stereo { level } else { level + 1 };
                let mid_args = BandArgs {
                    x,
                    y: None,
                    n,
                    blocks,
                    tf_change,
                    lowband,
                    level: next_level,
                    lm,
                    lowband_out: next_lowband_out1,
                    gain: if stereo { 1.0 } else { gain * mid },
                    fill,
                    ..a
                };
                let side_args = BandArgs {
                    x: yy,
                    y: None,
                    n,
                    blocks,
                    tf_change,
                    lowband: next_lowband2,
                    level: next_level,
                    lm,
                    lowband_out: None,
                    gain: gain * side,
                    fill: fill >> blocks,
                    ..a
                };
                let shift = if stereo { 0 } else { b0 >> 1 };
                let mut rebalance = *remaining_bits;
                if mbits >= sbits {
                    cm = self.quant_band(
                        enc,
                        BandArgs {
                            b: mbits,
                            ..mid_args
                        },
                        remaining_bits,
                    );
                    rebalance = mbits - (rebalance - *remaining_bits);
                    if rebalance > 3 << BITRES && itheta != 0 {
                        sbits += rebalance - (3 << BITRES);
                    }
                    cm |= self.quant_band(
                        enc,
                        BandArgs {
                            b: sbits,
                            ..side_args
                        },
                        remaining_bits,
                    ) << shift;
                } else {
                    cm = self.quant_band(
                        enc,
                        BandArgs {
                            b: sbits,
                            ..side_args
                        },
                        remaining_bits,
                    ) << shift;
                    rebalance = sbits - (rebalance - *remaining_bits);
                    if rebalance > 3 << BITRES && itheta != 16384 {
                        mbits += rebalance - (3 << BITRES);
                    }
                    cm |= self.quant_band(
                        enc,
                        BandArgs {
                            b: mbits,
                            ..mid_args
                        },
                        remaining_bits,
                    );
                }
            }
        } else {
            let mut q = bits2pulses(i, lm, b);
            let mut curr_bits = pulses2bits(i, lm, q);
            *remaining_bits -= curr_bits;
            while *remaining_bits < 0 && q > 0 {
                *remaining_bits += curr_bits;
                q -= 1;
                curr_bits = pulses2bits(i, lm, q);
                *remaining_bits -= curr_bits;
            }
            if q != 0 {
                let k = get_pulses(q) as usize;
                cm = self.alg_quant(enc, x, n, k, spread, blocks, gain);
            } else {
                // No pulses: the decoder folds or fills with noise, and the
                // encoder has to reproduce exactly what it will get, PRNG
                // state included, or every later band folds from the wrong
                // spectrum.
                let cm_mask = (1u32 << blocks) - 1;
                fill &= cm_mask;
                if fill == 0 {
                    self.x[x..x + n].fill(0.0);
                } else if lowband == Lowband::None {
                    for j in 0..n {
                        self.rng = celt_lcg_rand(self.rng);
                        self.x[x + j] = ((self.rng as i32) >> 20) as f32;
                    }
                    cm = cm_mask;
                    renormalise(&mut self.x[x..x + n], gain);
                } else {
                    for j in 0..n {
                        self.rng = celt_lcg_rand(self.rng);
                        let tmp = if self.rng & 0x8000 != 0 {
                            1.0 / 256.0
                        } else {
                            -1.0 / 256.0
                        };
                        self.x[x + j] = self.lowband(lowband, j) + tmp;
                    }
                    cm = fill;
                    renormalise(&mut self.x[x..x + n], gain);
                }
            }
        }

        if stereo {
            if n != 2 {
                stereo_merge(&mut self.x, x, y.expect("stereo"), mid, n);
            }
            if inv {
                let yy = y.expect("stereo");
                for j in 0..n {
                    self.x[yy + j] = -self.x[yy + j];
                }
            }
        } else if level == 0 {
            if b0 > 1 {
                interleave_hadamard(
                    &mut self.x[x..x + n0],
                    &mut self.hadamard_tmp,
                    n_b >> recombine,
                    b0 << recombine,
                    long_blocks,
                );
            }
            n_b = n_b0;
            let mut bb = b0;
            for _ in 0..time_divide {
                bb >>= 1;
                n_b <<= 1;
                cm |= cm >> bb;
                haar1(&mut self.x[x..], n_b, bb);
            }
            for k in 0..recombine {
                const BIT_DEINTERLEAVE: [u8; 16] = [
                    0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0,
                    0xF3, 0xFC, 0xFF,
                ];
                cm = BIT_DEINTERLEAVE[(cm & 0xF) as usize] as u32;
                haar1(&mut self.x[x..], n0 >> k, 1 << k);
            }
            bb <<= recombine;
            if let Some(o) = lowband_out {
                let nrm = (n0 as f32).sqrt();
                for j in 0..n0 {
                    self.norm[o + j] = nrm * self.x[x + j];
                }
            }
            cm &= (1 << bb) - 1;
        }
        cm
    }

    fn lowband(&self, lb: Lowband, j: usize) -> f32 {
        match lb {
            Lowband::None => 0.0,
            Lowband::Norm(l) => self.norm[l + j],
            Lowband::Scratch(o) => self.lowband_scratch[o + j],
        }
    }

    fn lowband_mut(&mut self, lb: Lowband) -> Option<&mut [f32]> {
        match lb {
            Lowband::None => None,
            Lowband::Norm(l) => Some(&mut self.norm[l..]),
            Lowband::Scratch(o) => Some(&mut self.lowband_scratch[o..]),
        }
    }

    /// Searches and codes one PVQ codeword: `k` pulses in `n` dimensions.
    #[allow(clippy::too_many_arguments)]
    fn alg_quant(
        &mut self,
        enc: &mut RangeEncoder,
        x: usize,
        n: usize,
        k: usize,
        spread: usize,
        b_blocks: usize,
        gain: f32,
    ) -> u32 {
        // The search runs in the rotated domain the decoder will rotate back.
        exp_rotation(&mut self.x[x..x + n], n, 1, b_blocks, k, spread);
        let yy = {
            let src = &self.x[x..x + n];
            let xa = &mut self.xa[..n];
            for (d, s) in xa.iter_mut().zip(src.iter()) {
                *d = s.abs();
            }
            pvq_search(xa, k, &mut self.iy[..n])
        };
        for j in 0..n {
            if self.x[x + j] < 0.0 {
                self.iy[j] = -self.iy[j];
            }
        }
        if self.urow.len() < k + 2 {
            self.urow.resize(k + 2, 0);
        }
        encode_pulses(enc, &self.iy[..n], n, k, &mut self.urow);

        let g = gain / yy.sqrt();
        for j in 0..n {
            self.x[x + j] = g * self.iy[j] as f32;
        }
        exp_rotation(&mut self.x[x..x + n], n, -1, b_blocks, k, spread);
        if b_blocks <= 1 {
            return 1;
        }
        let n0 = n / b_blocks;
        let mut mask = 0u32;
        for bi in 0..b_blocks {
            for j in 0..n0 {
                mask |= u32::from(self.iy[bi * n0 + j] != 0) << bi;
            }
        }
        mask
    }
}

/// Two-state Viterbi over the TF toggle chain: the coded bit flips the running
/// resolution, so a change costs `logp` bits and has to earn them back.
fn tf_viterbi(
    metric: &[[f32; 2]; NB_BANDS],
    start: usize,
    end: usize,
    transient: bool,
) -> (f32, [i32; NB_BANDS], bool) {
    let mut res = [0i32; NB_BANDS];
    if end <= start {
        return (0.0, res, false);
    }
    let switch_cost = if transient { 2.0 } else { 4.0 };
    let mut cost = [metric[start][0], metric[start][1] + switch_cost];
    let mut from = [[0usize; 2]; NB_BANDS];
    for i in start + 1..end {
        let sw = if transient { 4.0 } else { 5.0 };
        let mut next = [0.0f32; 2];
        for s in 0..2 {
            let stay = cost[s];
            let flip = cost[1 - s] + sw;
            let (c, f) = if stay <= flip {
                (stay, s)
            } else {
                (flip, 1 - s)
            };
            next[s] = c + metric[i][s];
            from[i][s] = f;
        }
        cost = next;
    }
    let mut s = usize::from(cost[1] < cost[0]);
    let best = cost[s];
    let mut changed = false;
    for i in (start..end).rev() {
        res[i] = s as i32;
        changed |= s == 1;
        s = from[i][s];
    }
    (best, res, changed)
}

/// The pulse allocation: greedy on `(x.y)^2/(y.y)`, then one relocation pass.
///
/// The greedy step is optimal for one pulse at a time; the relocation pass
/// catches the cases where an early pulse was placed on a coefficient that a
/// later one made redundant. Returns `y.y`.
fn pvq_search(xa: &[f32], k: usize, iy: &mut [i32]) -> f32 {
    let n = xa.len();
    iy.fill(0);
    if k == 0 {
        return 1.0;
    }
    let sum: f32 = xa.iter().sum();
    if sum <= 1e-20 || !sum.is_finite() {
        // A band with no energy at all: any codeword will do, and this one is
        // the cheapest to code.
        iy[0] = k as i32;
        return (k * k) as f32;
    }
    let mut yy = 0.0f32;
    let mut xy = 0.0f32;
    let mut left = k as i32;
    if k > n >> 1 {
        // Project first, so the greedy loop only places what rounding lost.
        let rcp = (k as f32 - 1.0) / sum;
        for j in 0..n {
            let v = (xa[j] * rcp).floor().max(0.0) as i32;
            iy[j] = v;
            yy += (v * v) as f32;
            xy += v as f32 * xa[j];
            left -= v;
        }
    }
    for _ in 0..left.max(0) {
        let mut best_j = 0usize;
        let mut best_num = -1.0f32;
        let mut best_den = 1.0f32;
        for j in 0..n {
            let num = xy + xa[j];
            let den = yy + 2.0 * iy[j] as f32 + 1.0;
            if num > 0.0 && num * num * best_den > best_num * best_num * den {
                best_num = num;
                best_den = den;
                best_j = j;
            }
        }
        yy += 2.0 * iy[best_j] as f32 + 1.0;
        xy += xa[best_j];
        iy[best_j] += 1;
    }
    // Relocation: move one pulse where it buys more. Bounded work, and the
    // objective never decreases.
    if n <= 256 {
        for j in 0..n {
            if iy[j] <= 0 {
                continue;
            }
            let xy0 = xy - xa[j];
            let yy0 = yy - (2.0 * iy[j] as f32 - 1.0);
            let mut best_j = j;
            let mut best_num = xy0 + xa[j];
            let mut best_den = yy0 + 2.0 * (iy[j] - 1) as f32 + 1.0;
            for t in 0..n {
                if t == j {
                    continue;
                }
                let num = xy0 + xa[t];
                let den = yy0 + 2.0 * iy[t] as f32 + 1.0;
                if num > 0.0 && num * num * best_den > best_num * best_num * den {
                    best_num = num;
                    best_den = den;
                    best_j = t;
                }
            }
            if best_j != j {
                iy[j] -= 1;
                yy = yy0 + 2.0 * iy[best_j] as f32 + 1.0;
                xy = xy0 + xa[best_j];
                iy[best_j] += 1;
            }
        }
    }
    let mut ryy = 0.0f32;
    for &v in iy.iter() {
        ryy += (v * v) as f32;
    }
    ryy.max(1e-15)
}

/// The index of a pulse vector within `V(n,k)`, the inverse of the decoder's
/// `decode_pulses`.
fn encode_pulses(enc: &mut RangeEncoder, y: &[i32], n: usize, k: usize, u: &mut [u32]) {
    let v = pvq_urow(n, k, u);
    let mut idx = 0u32;
    let mut k_left = k;
    for &val in y.iter().take(n) {
        let mag = val.unsigned_abs() as usize;
        let k0 = k_left;
        k_left = k0 - mag;
        idx = idx.wrapping_add(u[k_left]);
        if val < 0 {
            idx = idx.wrapping_add(u[k0 + 1]);
        }
        uprev(&mut u[..k_left + 2], 0);
    }
    enc.enc_uint(idx, v.max(2));
}

/// `ec_laplace_encode()`: the inverse of the decoder's `laplace_decode`.
/// Returns the value actually coded, which is clamped when the model runs out
/// of probability space.
fn laplace_encode(enc: &mut RangeEncoder, value: i32, fs0: u32, decay: i32) -> i32 {
    const MINP: u32 = 1;
    const NMIN: u32 = 16;
    let mut fl = 0u32;
    let mut fs = fs0;
    let mut coded = value;
    let a = value.unsigned_abs();
    if a > 0 {
        let mut k = 1u32;
        fl = fs0;
        let ft = 32768 - MINP * (2 * NMIN) - fs0;
        fs = ((ft * (16384 - decay as u32)) >> 15) + MINP;
        while k < a && fs > MINP {
            let f2 = 2 * fs;
            fl += f2;
            fs = (((f2 - 2 * MINP) * decay as u32) >> 15) + MINP;
            k += 1;
        }
        if k < a {
            // The tail, where every further step costs the minimum probability.
            let room = (32768 - fl).saturating_sub(2 * MINP) / (2 * MINP);
            let di = (a - k).min(room);
            fl += 2 * di * MINP;
            k += di;
        }
        coded = if value < 0 { -(k as i32) } else { k as i32 };
        if value > 0 {
            fl += fs;
        }
    }
    enc.encode_bin(fl, (fl + fs).min(32768), 15);
    coded
}

/// The coarse energy pass, shared by the intra and inter attempts.
#[allow(clippy::too_many_arguments)]
fn coarse_energy(
    enc: &mut RangeEncoder,
    old: &mut [f32],
    band_log_e: &[f32],
    err: &mut [f32],
    start: usize,
    end: usize,
    intra: bool,
    c: usize,
    lm: usize,
    budget: i32,
) {
    let model = &E_PROB_MODEL[lm][usize::from(intra)];
    let (coef, beta) = if intra {
        (0.0, BETA_INTRA)
    } else {
        (PRED_COEF[lm], BETA_COEF[lm])
    };
    let mut prev = [0.0f32; 2];
    for i in start..end {
        for (ch, prev) in prev.iter_mut().enumerate().take(c) {
            let tell = enc.tell() as i32;
            let k = i + ch * NB_BANDS;
            let old_v = old[k].max(-9.0);
            let pred = coef * old_v + *prev;
            let mut want = ((band_log_e[k] - pred).round() as i32).clamp(-24, 24);
            // Leave the bands above this one three bits each. A frame whose
            // energy collapses wants a large negative step in every band and
            // the Laplace model charges dearly for those, so without a reserve
            // the coarse energy can eat the frame and leave the bands above it
            // on the one-bit path, whose "keep the prediction" energy is then
            // far too loud. A guard, not a gain: measured neutral on speech and
            // on music across 16..510 kbps, which is what a guard should be.
            let reserved = 3 * c as i32 * (end - i) as i32;
            let bits_left = budget - tell - reserved;
            if i != start && bits_left < 30 {
                if bits_left < 24 {
                    want = want.min(1);
                }
                if bits_left < 16 {
                    want = want.max(-1);
                }
            }
            let want = want;
            let qi = if budget - tell >= 15 {
                let pi = 2 * i.min(20);
                laplace_encode(
                    enc,
                    want,
                    (model[pi] as u32) << 7,
                    (model[pi + 1] as i32) << 6,
                )
            } else if budget - tell >= 2 {
                let qi = want.clamp(-1, 1);
                let s = match qi {
                    0 => 0,
                    -1 => 1,
                    _ => 2,
                };
                enc.enc_icdf(s, &SMALL_ENERGY_ICDF, 2);
                qi
            } else if budget - tell >= 1 {
                let qi = want.clamp(-1, 0);
                enc.enc_bit_logp(qi < 0, 1);
                qi
            } else {
                -1
            };
            let q = qi as f32;
            old[k] = pred + q;
            *prev += q - beta * q;
            err[k] = band_log_e[k] - old[k];
        }
    }
}

/// Writes the three coded allocation decisions.
struct EncAlloc<'a> {
    enc: &'a mut RangeEncoder,
    /// Where intensity stereo should start, in absolute band numbers.
    intensity: usize,
    dual: bool,
}

impl AllocCoder for EncAlloc<'_> {
    fn skip(&mut self, ctx: SkipCtx) -> bool {
        // Skipping folds a band instead of coding it. Below roughly half a bit
        // per coefficient a coded band is worse than the fold, so the top bands
        // are dropped until one is worth its bits; the two lowest coded bands
        // are never dropped, whatever the budget.
        let depth = if ctx.coded_bands > 17 { 9 } else { 7 };
        let floor = (depth * ctx.band_width) << ctx.lm << BITRES >> 4;
        let code_it = ctx.band <= ctx.start + 1 || ctx.band_bits > floor;
        self.enc.enc_bit_logp(code_it, 1);
        code_it
    }

    fn intensity(&mut self, ft: u32) -> u32 {
        let v = (self.intensity as u32).min(ft - 1);
        self.enc.enc_uint(v, ft);
        v
    }

    fn dual_stereo(&mut self) -> bool {
        self.enc.enc_bit_logp(self.dual, 1);
        self.dual
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::celt::CeltDecoder;
    use crate::range::RangeDecoder;

    /// Forward MDCT into the decoder's inverse, overlap-added: the signal must
    /// come back at unity gain. This pins the transform's scale, its window
    /// alignment and the frame offset all at once.
    #[test]
    fn mdct_round_trip_is_unity_gain() {
        for &l in &[120usize, 240, 480, 960] {
            let frames = 6;
            let total = frames * l;
            let sig: Vec<f32> = (0..total + l)
                .map(|i| (i as f32 * 0.031).sin() * 0.6 + (i as f32 * 0.0071).sin() * 0.35)
                .collect();
            let window = celt::overlap_window();
            let mut fwd = MdctPlan::new(l);
            let mut inv_plan = crate::celt::test_imdct_plan(l);
            let mut out = vec![0.0f32; total + 2 * l];
            let mut hist = vec![0.0f32; OVERLAP];
            for t in 0..frames {
                let mut inp = vec![0.0f32; l + OVERLAP];
                inp[..OVERLAP].copy_from_slice(&hist);
                inp[OVERLAP..].copy_from_slice(&sig[t * l..t * l + l]);
                hist.copy_from_slice(&sig[t * l + l - OVERLAP..t * l + l]);
                let mut spec = vec![0.0f32; l];
                fwd.forward(&inp, &window, &mut spec, 1);
                inv_plan.inverse(&spec, 1, &window, &mut out, t * l);
            }
            // The decoder's output frame t is out[t*l .. t*l+l]; it reproduces
            // the input delayed by one overlap.
            let lo = l + OVERLAP;
            let hi = (frames - 1) * l;
            let mut num = 0.0f64;
            let mut den = 0.0f64;
            let mut err = 0.0f64;
            for i in lo..hi {
                let want = sig[i - OVERLAP] as f64;
                num += out[i] as f64 * want;
                den += want * want;
                err += (out[i] as f64 - want) * (out[i] as f64 - want);
            }
            let gain = num / den;
            assert!(
                (gain - 1.0).abs() < 1e-3,
                "L={l}: round-trip gain {gain}, expected 1"
            );
            assert!(
                (err / den).sqrt() < 1e-3,
                "L={l}: reconstruction error {}",
                (err / den).sqrt()
            );
        }
    }

    /// The pulse index is the exact inverse of the decoder's, over every shape
    /// the search can produce.
    #[test]
    fn pulse_index_round_trips() {
        /// `V(n,k)` in 64 bits, to spot the shapes a 32-bit index cannot hold.
        /// The codebook split in `quant_band` keeps every coded band under 32
        /// bits, so those combinations never reach the coder.
        fn v64(
            n: usize,
            k: usize,
            memo: &mut std::collections::HashMap<(usize, usize), u64>,
        ) -> u64 {
            if k == 0 {
                return 1;
            }
            if n == 0 {
                return 0;
            }
            if let Some(&v) = memo.get(&(n, k)) {
                return v;
            }
            let v = v64(n - 1, k, memo) + v64(n, k - 1, memo) + v64(n - 1, k - 1, memo);
            memo.insert((n, k), v);
            v
        }
        let mut memo = std::collections::HashMap::new();
        let mut u = vec![0u32; 512];
        let mut seed = 12345u32;
        for n in [2usize, 3, 8, 16, 20] {
            for k in [1usize, 2, 5, 9, 17] {
                if k + 2 > u.len() || v64(n, k, &mut memo) >= 1u64 << 32 {
                    continue;
                }
                for _ in 0..20 {
                    // A random pulse vector of exactly k pulses.
                    let mut y = vec![0i32; n];
                    let mut left = k as i32;
                    while left > 0 {
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        let j = (seed >> 16) as usize % n;
                        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                        let s = if seed & 0x10000 != 0 { 1 } else { -1 };
                        if y[j] != 0 && y[j].signum() != s {
                            continue;
                        }
                        y[j] += s;
                        left -= 1;
                    }
                    let mut enc = RangeEncoder::new(64);
                    encode_pulses(&mut enc, &y, n, k, &mut u);
                    let frame = enc.done();
                    let mut dec = RangeDecoder::new(&frame);
                    let mut got = vec![0i32; n];
                    crate::celt::test_decode_pulses(&mut dec, n, k, &mut got, &mut u);
                    assert_eq!(got, y, "n={n} k={k}");
                }
            }
        }
    }

    /// The Laplace coder round-trips every value the coarse energy can produce,
    /// including the clamped tail.
    #[test]
    fn laplace_round_trips() {
        for &fs0 in &[(24u32) << 7, (72) << 7, (200) << 7] {
            for &decay in &[10i32 << 6, 96 << 6, 179 << 6] {
                for v in -30i32..=30 {
                    let mut enc = RangeEncoder::new(64);
                    let coded = laplace_encode(&mut enc, v, fs0, decay);
                    let frame = enc.done();
                    let mut dec = RangeDecoder::new(&frame);
                    let got = crate::celt::test_laplace_decode(&mut dec, fs0, decay);
                    assert_eq!(got, coded, "v={v} fs0={fs0} decay={decay}");
                    assert!(
                        coded.signum() == v.signum() || v == 0,
                        "clamping must not change the sign"
                    );
                    assert!(coded.abs() <= v.abs());
                }
            }
        }
    }

    /// A pure tone through encoder and decoder: the decoded frame has to
    /// correlate with the input, which is the smallest end-to-end proof that
    /// every coded decision agrees between the two.
    #[test]
    fn celt_round_trip_correlates() {
        let n = 960usize;
        let frames = 20;
        let sig: Vec<f32> = (0..n * frames)
            .map(|i| {
                0.4 * (i as f32 * 0.05).sin()
                    + 0.3 * (i as f32 * 0.131).sin()
                    + 0.2 * (i as f32 * 0.301).sin()
            })
            .collect();
        let mut e = CeltEncoder::new(1, 1);
        let mut d = CeltDecoder::new(1, 1);
        let mut out = vec![0.0f32; n * frames];
        for t in 0..frames {
            let mut enc = RangeEncoder::new(160);
            e.encode(&sig[t * n..(t + 1) * n], n, 21, &mut enc).unwrap();
            assert!(!enc.error(), "frame {t} overran its budget");
            let frame = enc.done();
            let mut dec = RangeDecoder::new(&frame);
            d.decode(&mut dec, &mut out[t * n..(t + 1) * n], n, 0, 21, 1)
                .unwrap();
            assert_eq!(dec.range(), e.rng(), "frame {t}: range coder desynced");
        }
        // Skip the first frames (the decoder's overlap has to fill).
        let lo = 2 * n;
        let mut num = 0.0f64;
        let mut da = 0.0f64;
        let mut db = 0.0f64;
        for i in lo..n * frames {
            let a = out[i] as f64;
            let b = sig[i - OVERLAP] as f64;
            num += a * b;
            da += a * a;
            db += b * b;
        }
        let corr = num / (da.sqrt() * db.sqrt());
        assert!(corr > 0.95, "correlation {corr}");
    }
}

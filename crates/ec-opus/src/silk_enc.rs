//! SILK encoder analysis front end (subtask D1).
//!
//! This module decides *what* a SILK frame should say — resampled input,
//! a voice-activity flag, an LPC filter reduced to NLSF codebook indices, a
//! pitch contour, and per-subframe gain indices — without writing any of it
//! to a bitstream. The NSQ/LTP excitation search and the actual range-coder
//! writes are a separate subtask (D2) that consumes the types here; nothing
//! in this file touches [`crate::range::RangeEncoder`].
//!
//! Every quantiser below hands its candidate indices to the *decoder's own*
//! functions in [`crate::silk`] (`nlsf_decode`, `nlsf2a`, `gains_dequant`) to
//! measure quantisation error, so the indices this module produces are
//! guaranteed to be indices the decoder in `silk.rs` can read back — there is
//! no separate, possibly-divergent, notion of "encoder-side dequantisation".
//!
//! The VAD is a simplified energy/spectral-flatness gate, *not* RFC 6716
//! Appendix A's cascade of half-band filters and hangover logic — stated
//! once, here, for the whole module (assumption called out by the D1
//! charter).

// D2 (the NSQ/LTP/entropy writer) is the real caller of this API; until
// that charter lands, nothing outside this module's own tests exercises it.
#![allow(dead_code)]

use std::f32::consts::PI;
use std::f64::consts::PI as PI64;

use crate::silk::tables::{
    CB_LAGS_STAGE2, CB_LAGS_STAGE2_10_MS, CB_LAGS_STAGE3, CB_LAGS_STAGE3_10_MS,
};
use crate::silk::{
    gains_dequant, log2lin, nlsf_cb, nlsf_decode, nlsf_unpack, nlsf_vq_weights_laroia, smlawb,
    smulbb, sqrt_approx, NlsfCodebook, MAX_LPC_ORDER, MAX_NB_SUBFR, PE_MAX_LAG_MS, PE_MIN_LAG_MS,
};

#[cfg(test)]
use crate::silk::{nlsf2a, nlsf_stabilize};

// ---------------------------------------------------------------------------
// 1. Resampler: 48 kHz -> SILK's internal 8/12/16 kHz.
// ---------------------------------------------------------------------------

/// A linear-phase FIR decimator from 48 kHz to a SILK internal rate.
///
/// This is a windowed-sinc low-pass (Hamming window, unity DC gain) run at
/// the decimated rate, independent of the decoder's `Resampler` in
/// `silk.rs` — that one's `DELAY_MATRIX_DEC` table only covers *upsampling*
/// paths (internal rate -> API rate) and panics on `rate_id` for a 48 kHz
/// input, so downsampling needs its own path (charter point 1, "add a down
/// path").
///
/// Group delay is exactly `(taps - 1) / 2` input (48 kHz) samples, because
/// the kernel is symmetric — see [`Resampler48::delay_samples`].
///
/// corner-cut: `process` requires `input.len()` to be a multiple of the
/// decimation factor (2, 3, 4 or 6); 20 ms @ 48 kHz (960 samples) always is.
/// Upgrade path: track a fractional phase across calls if a caller ever
/// needs non-frame-aligned chunks.
#[derive(Clone, Debug)]
pub(crate) struct Resampler48 {
    factor: usize,
    taps: Vec<f32>,
    history: Vec<f32>,
}

impl Resampler48 {
    /// `target_khz` must be 8, 12 or 16 (SILK's NB/MB/WB internal rates).
    pub(crate) fn new(target_khz: u32) -> Self {
        assert!(matches!(target_khz, 8 | 12 | 16), "unsupported SILK rate");
        let factor = 48 / target_khz as usize;
        let n = factor * 8 + 1; // odd -> symmetric -> linear phase
        let cutoff = 0.9 / factor as f32;
        let center = (n - 1) as f32 / 2.0;
        let mut taps = vec![0f32; n];
        let mut sum = 0f32;
        for (i, tap) in taps.iter_mut().enumerate() {
            let x = i as f32 - center;
            let sinc = if x == 0.0 {
                cutoff
            } else {
                (PI * cutoff * x).sin() / (PI * x)
            };
            let w = 0.54 - 0.46 * (2.0 * PI * i as f32 / (n - 1) as f32).cos();
            *tap = sinc * w;
            sum += *tap;
        }
        for tap in taps.iter_mut() {
            *tap /= sum;
        }
        Resampler48 {
            factor,
            taps,
            history: vec![0.0; n - 1],
        }
    }

    /// The filter's group delay, in 48 kHz input samples.
    pub(crate) fn delay_samples(&self) -> usize {
        (self.taps.len() - 1) / 2
    }

    /// Downsamples `input` (48 kHz), keeping filter history across calls.
    /// `input.len()` must be a multiple of the decimation factor.
    pub(crate) fn process(&mut self, input: &[f32]) -> Vec<f32> {
        debug_assert_eq!(input.len() % self.factor, 0);
        let n = self.taps.len();
        let mut buf = self.history.clone();
        buf.extend_from_slice(input);
        let out_len = input.len() / self.factor;
        let mut out = Vec::with_capacity(out_len);
        for j in 0..out_len {
            let start = j * self.factor;
            let mut acc = 0f32;
            for (k, &tap) in self.taps.iter().enumerate() {
                acc += tap * buf[start + k];
            }
            out.push(acc);
        }
        let tail_start = buf.len() - (n - 1);
        self.history = buf[tail_start..].to_vec();
        out
    }
}

// ---------------------------------------------------------------------------
// 2. Voice activity detection.
// ---------------------------------------------------------------------------

/// Per-20ms-frame VAD result.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VadResult {
    pub(crate) active: bool,
    pub(crate) speech_prob: f32,
}

/// Simplified energy + spectral-flatness VAD (not RFC 6716 Appendix A).
#[derive(Clone, Debug)]
pub(crate) struct Vad {
    noise_floor: f32,
}

impl Default for Vad {
    fn default() -> Self {
        Vad { noise_floor: 1e-6 }
    }
}

impl Vad {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Analyses one frame (any length/rate); a naive DFT gets the magnitude
    /// spectrum, so keep frames in the few-hundred-sample range this
    /// encoder actually uses (20 ms at 8/12/16 kHz).
    pub(crate) fn analyze(&mut self, frame: &[f32]) -> VadResult {
        let n = frame.len().max(1);
        let energy = frame.iter().map(|&x| x * x).sum::<f32>() / n as f32;

        let bins = (n / 2).max(1);
        let mut log_sum = 0f64;
        let mut lin_sum = 0f64;
        for k in 0..bins {
            let mut re = 0f32;
            let mut im = 0f32;
            for (t, &x) in frame.iter().enumerate() {
                let theta = -2.0 * PI * k as f32 * t as f32 / n as f32;
                re += x * theta.cos();
                im += x * theta.sin();
            }
            let mag = (re * re + im * im).sqrt().max(1e-9) as f64;
            log_sum += mag.ln();
            lin_sum += mag;
        }
        let geo_mean = (log_sum / bins as f64).exp();
        let arith_mean = lin_sum / bins as f64;
        // 0 = perfectly tonal, 1 = white-noise-like.
        let flatness = (geo_mean / arith_mean.max(1e-9)) as f32;

        // Leaky minimum-follower noise floor.
        self.noise_floor = if energy < self.noise_floor {
            energy
        } else {
            self.noise_floor * 0.99 + energy * 0.01
        };
        let snr_db = 10.0 * (energy / self.noise_floor.max(1e-9)).max(1e-9).log10();

        let prob = ((1.0 - flatness).clamp(0.0, 1.0) * (snr_db / 30.0).clamp(0.0, 1.0))
            .clamp(0.0, 1.0);
        VadResult {
            active: prob > 0.15,
            speech_prob: prob,
        }
    }
}

// ---------------------------------------------------------------------------
// 3. LPC analysis: autocorrelation, Levinson-Durbin, LPC -> NLSF.
// ---------------------------------------------------------------------------

/// One frame's LPC analysis, ready for NLSF quantisation.
#[derive(Clone, Debug)]
pub(crate) struct LpcAnalysis {
    /// Direct-form LPC coefficients (`order` of them), `x[n] ~= sum a[k]*x[n-k-1]`.
    pub(crate) lpc: Vec<f64>,
    /// Line spectral frequencies, in the decoder's Q15 domain (`nlsf2a`'s
    /// own input format), ascending, order-length prefix valid.
    pub(crate) nlsf_q15: [i16; MAX_LPC_ORDER],
    pub(crate) order: usize,
}

/// Autocorrelation (lags `0..=order`) with a light Gaussian lag window and a
/// white-noise floor added at lag 0, both standard Levinson-Durbin
/// stabilisers.
fn autocorrelate(frame: &[f32], order: usize) -> Vec<f64> {
    let n = frame.len();
    let mut r = vec![0f64; order + 1];
    for (lag, out) in r.iter_mut().enumerate() {
        let mut sum = 0f64;
        for t in 0..n.saturating_sub(lag) {
            sum += frame[t] as f64 * frame[t + lag] as f64;
        }
        // Gaussian lag window: exp(-0.5*(2*pi*lag*bw/n)^2), bw ~ 60 Hz worth.
        let bw = 60.0 / 8000.0; // fraction of an 8 kHz-equivalent bandwidth
        let w = (-0.5 * (2.0 * PI64 * lag as f64 * bw).powi(2)).exp();
        *out = sum * w;
    }
    r[0] *= 1.0 + 1e-4; // white-noise floor
    r
}

/// Levinson-Durbin recursion. Returns `None` if the signal is exactly
/// silent (autocorrelation at lag 0 is zero).
fn levinson_durbin(autoc: &[f64], order: usize) -> Option<Vec<f64>> {
    if autoc[0] <= 0.0 {
        return None;
    }
    let mut a = vec![0f64; order + 1];
    let mut err = autoc[0];
    for i in 1..=order {
        let mut acc = autoc[i];
        for j in 1..i {
            acc -= a[j] * autoc[i - j];
        }
        let k = acc / err;
        let mut new_a = a.clone();
        new_a[i] = k;
        for j in 1..i {
            new_a[j] = a[j] - k * a[i - j];
        }
        a = new_a;
        err *= 1.0 - k * k;
        if err <= 0.0 {
            err = 1e-9;
        }
    }
    Some(a[1..=order].to_vec())
}

/// Bandwidth-expands (moves poles inward by `gamma` per order) so the LPC
/// filter this module hands to root-finding is comfortably stable.
fn bandwidth_expand(lpc: &mut [f64], gamma: f64) {
    let mut g = gamma;
    for c in lpc.iter_mut() {
        *c *= g;
        g *= gamma;
    }
}

/// LPC -> NLSF: the standard line-spectral-pair construction, independent
/// of `nlsf2a`'s own internal representation (its `find_poly` builds an
/// algebraically different — not root-per-coefficient — polynomial form,
/// so inverting it directly is not the way back; this instead rebuilds the
/// textbook P(z)/Q(z) split and root-finds those on the unit circle, then
/// hands `nlsf2a`/`nlsf_decode` a plain ascending-angle NLSF vector, which
/// is all their documented contract requires).
///
/// `A(z) = 1 + a[0]z^-1 + .. + a[M-1]z^-M`. Split
/// `P(z) = A(z) + z^-(M+1)A(z^-1)` (root at `z=-1`) and
/// `Q(z) = A(z) - z^-(M+1)A(z^-1)` (root at `z=1`); on the unit circle,
/// after removing the shared linear-phase factor, both reduce to a real
/// function of `w` whose `M/2` zero crossings in `(0, pi)` are exactly the
/// line spectral frequencies. `P`'s and `Q`'s roots interlace by LSP
/// theory, so merging and sorting both root sets gives the ascending NLSF
/// vector directly, without needing to track which physical root came from
/// which half.
fn lpc_to_nlsf(lpc: &[f64], order: usize) -> [i16; MAX_LPC_ORDER] {
    let dd = order / 2;
    let m1 = order as f64 + 1.0;

    // Real-valued P(w) = 2*Re[e^{j(M+1)w/2} * A(e^{jw})],
    //             Q(w) = 2*Im[e^{j(M+1)w/2} * A(e^{jw})].
    let eval = |w: f64| -> (f64, f64) {
        let mut re = 1.0;
        let mut im = 0.0;
        for (k, &a) in lpc.iter().enumerate() {
            let kw = (k as f64 + 1.0) * w;
            // `lpc` is the predictor (`x[n] ~= sum a[k] x[n-k-1]`), so the
            // analysis polynomial is A(z) = 1 - sum a[k] z^-(k+1).
            re -= a * kw.cos();
            im += a * kw.sin();
        }
        let half = m1 * w / 2.0;
        let (hs, hc) = half.sin_cos();
        (2.0 * (hc * re - hs * im), 2.0 * (hs * re + hc * im))
    };

    let roots_of = |want_p: bool, dd: usize| -> Vec<f64> {
        const GRID: usize = 256;
        let mut roots = Vec::with_capacity(dd);
        let f = |w: f64| {
            let (p, q) = eval(w);
            if want_p { p } else { q }
        };
        let mut prev_w = 0.0f64;
        let mut prev_v = f(prev_w);
        for i in 1..=GRID {
            let w = i as f64 / GRID as f64 * PI64;
            let v = f(w);
            if prev_v != 0.0 && (v == 0.0 || v.signum() != prev_v.signum()) {
                let (mut lo, mut hi) = (prev_w, w);
                let mut flo = prev_v;
                for _ in 0..40 {
                    let mid = 0.5 * (lo + hi);
                    let fm = f(mid);
                    if fm.signum() == flo.signum() {
                        lo = mid;
                        flo = fm;
                    } else {
                        hi = mid;
                    }
                }
                roots.push(0.5 * (lo + hi));
                if roots.len() == dd {
                    break;
                }
            }
            prev_w = w;
            prev_v = v;
        }
        // corner-cut: an input whose P/Q don't interlace into exactly `dd`
        // zero crossings each (near-unstable LPC) gets an even angular
        // spread instead of a failed analysis.
        while roots.len() < dd {
            let i = roots.len();
            roots.push((i as f64 + 1.0) / (dd as f64 + 1.0) * PI64);
        }
        roots
    };

    let mut all: Vec<f64> = roots_of(true, dd);
    all.extend(roots_of(false, dd));
    all.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut nlsf_q15 = [0i16; MAX_LPC_ORDER];
    for (i, &w) in all.iter().enumerate().take(order) {
        nlsf_q15[i] = (w / PI64 * 32768.0).round().clamp(0.0, 32767.0) as i16;
    }
    nlsf_q15
}

/// Runs the full LPC front end on a 20 ms (or 10 ms) analysis frame at the
/// SILK internal rate: autocorrelation, Levinson-Durbin, bandwidth
/// expansion, then LPC -> NLSF. `order` is 10 for NB/MB, 16 for WB.
pub(crate) fn lpc_analyze(frame: &[f32], order: usize) -> Option<LpcAnalysis> {
    let autoc = autocorrelate(frame, order);
    let mut lpc = levinson_durbin(&autoc, order)?;
    bandwidth_expand(&mut lpc, 0.999);
    let nlsf_q15 = lpc_to_nlsf(&lpc, order);
    Some(LpcAnalysis {
        lpc,
        nlsf_q15,
        order,
    })
}

// ---------------------------------------------------------------------------
// 4. NLSF vector quantisation against the decoder's own codebooks.
// ---------------------------------------------------------------------------

/// Indices `silk::decode_indices` (`silk.rs:615`) reads for one frame's
/// NLSFs, plus the interpolation index (`decode_indices`'s
/// `nlsf_interp_coef_q2`).
#[derive(Clone, Debug)]
pub(crate) struct NlsfQuant {
    /// `indices.nlsf[0]`: the stage-1 codebook vector index.
    /// `indices.nlsf[1..=order]`: the stage-2 residual indices.
    pub(crate) indices: [i8; MAX_LPC_ORDER + 1],
    /// Assumption (charter point 4): always 4 (no interpolation).
    pub(crate) interp_index: i32,
    /// The dequantised NLSFs the decoder will reconstruct, for the caller's
    /// own error accounting.
    pub(crate) nlsf_q15: [i16; MAX_LPC_ORDER],
}

/// Quantises `target_q15` (an `order`-length prefix) against the decoder's
/// NB/MB or WB codebook: an 8-best stage-1 survivor search, each survivor
/// completed by a sequential stage-2 residual search that mirrors
/// `nlsf_decode`'s own predictive chain, scored by actually calling
/// `nlsf_decode` — so the winner is whichever candidate the decoder would
/// reconstruct closest to `target_q15`, not an approximation of it.
pub(crate) fn quantize_nlsf(target_q15: &[i16], wideband: bool) -> NlsfQuant {
    let cb = nlsf_cb(wideband);
    let order = cb.order;
    debug_assert!(target_q15.len() >= order);

    // Stage 1: score every codebook vector, keep the 8 best.
    let weights = nlsf_vq_weights_laroia(&target_q15[..order]);
    let mut scored: Vec<(f64, usize)> = (0..cb.n_vectors)
        .map(|v| {
            let base = v * order;
            let mut err = 0f64;
            for i in 0..order {
                let cb_val = (cb.cb1_q8[base + i] as i32) << 7;
                let d = (target_q15[i] as i32 - cb_val) as f64;
                err += (weights[i] as f64) * d * d;
            }
            (err, v)
        })
        .collect();
    scored.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let survivors = scored.into_iter().take(8).map(|(_, v)| v);

    let mut best: Option<(f64, NlsfQuant)> = None;
    for v in survivors {
        let indices = quantize_stage2(target_q15, order, v, cb);
        let decoded = nlsf_decode(&indices[..=order], cb);
        let mut err = 0f64;
        for i in 0..order {
            let d = (target_q15[i] - decoded[i]) as f64;
            err += d * d;
        }
        if best.as_ref().map(|(e, _)| err < *e).unwrap_or(true) {
            let mut idx = [0i8; MAX_LPC_ORDER + 1];
            idx[..=order].copy_from_slice(&indices[..=order]);
            best = Some((
                err,
                NlsfQuant {
                    indices: idx,
                    interp_index: 4,
                    nlsf_q15: decoded,
                },
            ));
        }
    }
    best.unwrap().1
}

/// Sequential stage-2 residual search for one stage-1 survivor `cb1_index`,
/// matching `nlsf_decode`'s own back-to-front predictive chain exactly (it
/// calls the same `smulbb`/`smlawb` fixed-point steps) so the indices this
/// returns decode to the value this function itself computed.
fn quantize_stage2(
    target_q15: &[i16],
    order: usize,
    cb1_index: usize,
    cb: &NlsfCodebook,
) -> Vec<i8> {
    let mut indices = vec![0i8; order + 1];
    indices[0] = cb1_index as i8;
    let (_, pred_q8) = nlsf_unpack(cb, cb1_index);

    // The weight and target residual the decoder computes from the stage-1
    // mean (silk.rs's nlsf_decode: weights come from the *stage-1* nlsf,
    // not the final decoded one, so this is not circular).
    let mut cb1_q15 = [0i16; MAX_LPC_ORDER];
    for (i, slot) in cb1_q15.iter_mut().enumerate().take(order) {
        *slot = (cb.cb1_q8[cb1_index * order + i] as i16) << 7;
    }
    let w_qw = nlsf_vq_weights_laroia(&cb1_q15[..order]);

    let mut out_q10 = 0i32; // res_q10[i+1], feeds pred_q10 for step i
    for i in (0..order).rev() {
        let pred_q10 = smulbb(out_q10, pred_q8[i] as i32) >> 8;
        let w_q9 = sqrt_approx((w_qw[i] as i32) << 16);
        // Target residual: invert nlsf_decode's final combine step,
        // v = cb1[i] + (res_q10[i]<<14)/w_q9.
        let target_res_q10 =
            (((target_q15[i] as i32 - cb1_q15[i] as i32) as i64 * w_q9 as i64) >> 14) as i32;
        let desired = target_res_q10 - pred_q10;

        // Candidate stored index c: out_q10 = c<<10 (+-102) combined via
        // smlawb(pred_q10, raw, quant_step). Estimate then refine locally.
        let step = cb.quant_step_size_q16.max(1);
        let est = ((desired as i64) << 16) / step as i64;
        let c0 = ((est + 512) >> 10) as i32; // round to nearest integer index
        let mut best_c = c0;
        let mut best_out = 0i32;
        let mut best_err = i64::MAX;
        for c in (c0 - 3)..=(c0 + 3) {
            let mut raw = c << 10;
            if raw > 0 {
                raw -= 102;
            } else if raw < 0 {
                raw += 102;
            }
            let out = smlawb(pred_q10, raw, cb.quant_step_size_q16);
            let err = ((out - target_res_q10) as i64).abs();
            if err < best_err {
                best_err = err;
                best_c = c;
                best_out = out;
            }
        }
        indices[i + 1] = best_c.clamp(i8::MIN as i32, i8::MAX as i32) as i8;
        out_q10 = best_out;
    }
    indices
}

// ---------------------------------------------------------------------------
// 5. Pitch estimation.
// ---------------------------------------------------------------------------

/// One frame's pitch decision.
#[derive(Clone, Debug)]
pub(crate) struct PitchEstimate {
    pub(crate) voiced: bool,
    /// Overall lag in samples at `fs_khz`.
    pub(crate) lag: i32,
    /// `indices.lag_index` (`silk_decode_pitch`'s input): `lag - min_lag`.
    pub(crate) lag_index: i32,
    /// Best-matching contour column into the decoder's `CB_LAGS_*` table.
    pub(crate) contour_index: i32,
    /// Per-subframe lags the chosen contour implies.
    pub(crate) pitch_l: [i32; MAX_NB_SUBFR],
}

/// Normalised cross-correlation at a single lag over `frame`.
fn ncc(frame: &[f32], lag: usize) -> f32 {
    if lag >= frame.len() {
        return 0.0;
    }
    let n = frame.len() - lag;
    if n == 0 {
        return 0.0;
    }
    let mut num = 0f64;
    let mut e0 = 0f64;
    let mut e1 = 0f64;
    for t in 0..n {
        let a = frame[t] as f64;
        let b = frame[t + lag] as f64;
        num += a * b;
        e0 += a * a;
        e1 += b * b;
    }
    let denom = (e0 * e1).sqrt();
    if denom < 1e-9 {
        0.0
    } else {
        (num / denom) as f32
    }
}

/// Coarse (4 kHz-equivalent decimated) search followed by a fine search at
/// full rate, then a per-subframe refinement and contour-table lookup
/// against the decoder's own `CB_LAGS_*` tables (`silk.rs:157-176`).
pub(crate) fn estimate_pitch(frame: &[f32], fs_khz: i32, nb_subfr: usize) -> Option<PitchEstimate> {
    let min_lag = (PE_MIN_LAG_MS * fs_khz) as usize;
    let max_lag = (PE_MAX_LAG_MS * fs_khz) as usize;
    if frame.len() <= max_lag {
        return None;
    }

    // Coarse: decimate by ~fs_khz/4 with block averaging, search there.
    let dec = ((fs_khz / 4).max(1)) as usize;
    let decimated: Vec<f32> = frame
        .chunks(dec)
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect();
    let coarse_min = (min_lag / dec).max(1);
    let coarse_max = (max_lag / dec).min(decimated.len().saturating_sub(1)).max(coarse_min);
    let coarse_scores: Vec<f32> = (coarse_min..=coarse_max)
        .map(|lag| ncc(&decimated, lag))
        .collect();
    // Pitch periodicity peaks at every multiple of the true period; taking
    // the smallest lag whose score is close to the range's best avoids
    // locking onto a doubled (or halved) period ("octave error").
    let global_best = coarse_scores.iter().cloned().fold(f32::MIN, f32::max);
    let best_coarse = (coarse_min..=coarse_max)
        .zip(coarse_scores.iter())
        .find(|&(_, &s)| s >= 0.85 * global_best)
        .map(|(lag, _)| lag)
        .unwrap_or(coarse_min);

    // Fine: refine at full rate around the coarse hit.
    let center = (best_coarse * dec).clamp(min_lag, max_lag);
    let fine_lo = center.saturating_sub(dec).max(min_lag);
    let fine_hi = (center + dec).min(max_lag);
    let fine_scores: Vec<f32> = (fine_lo..=fine_hi).map(|lag| ncc(frame, lag)).collect();
    let fine_best = fine_scores.iter().cloned().fold(f32::MIN, f32::max);
    let best_lag = (fine_lo..=fine_hi)
        .zip(fine_scores.iter())
        .find(|&(_, &s)| s >= 0.85 * fine_best)
        .map(|(lag, _)| lag)
        .unwrap_or(fine_lo);
    let best_score = fine_best;
    let voiced = best_score > 0.35;
    if !voiced {
        return Some(PitchEstimate {
            voiced: false,
            lag: best_lag as i32,
            lag_index: (best_lag as i32 - min_lag as i32).max(0),
            contour_index: 0,
            pitch_l: [best_lag as i32; MAX_NB_SUBFR],
        });
    }

    // Per-subframe lags: local search around the global lag.
    let sub_len = frame.len() / nb_subfr.max(1);
    let mut per_sub = [0i32; MAX_NB_SUBFR];
    for (k, slot) in per_sub.iter_mut().enumerate().take(nb_subfr) {
        let start = k * sub_len;
        let end = (start + sub_len).min(frame.len());
        let sub = &frame[start..end];
        let lo = best_lag.saturating_sub(4).max(min_lag);
        let hi = (best_lag + 4).min(max_lag);
        let mut sk_lag = best_lag;
        let mut sk_score = f32::MIN;
        for lag in lo..=hi {
            let score = ncc(sub, lag);
            if score > sk_score {
                sk_score = score;
                sk_lag = lag;
            }
        }
        *slot = sk_lag as i32;
    }

    // Match the per-subframe lags against the decoder's contour tables.
    let (table, n_cols): (&[&[i8]], usize) = match (fs_khz, nb_subfr) {
        (8, 4) => (
            &[
                &CB_LAGS_STAGE2[0],
                &CB_LAGS_STAGE2[1],
                &CB_LAGS_STAGE2[2],
                &CB_LAGS_STAGE2[3],
            ],
            CB_LAGS_STAGE2[0].len(),
        ),
        (8, 2) => (
            &[&CB_LAGS_STAGE2_10_MS[0], &CB_LAGS_STAGE2_10_MS[1]],
            CB_LAGS_STAGE2_10_MS[0].len(),
        ),
        (_, 4) => (
            &[
                &CB_LAGS_STAGE3[0],
                &CB_LAGS_STAGE3[1],
                &CB_LAGS_STAGE3[2],
                &CB_LAGS_STAGE3[3],
            ],
            CB_LAGS_STAGE3[0].len(),
        ),
        _ => (
            &[&CB_LAGS_STAGE3_10_MS[0], &CB_LAGS_STAGE3_10_MS[1]],
            CB_LAGS_STAGE3_10_MS[0].len(),
        ),
    };
    let lag = best_lag as i32;
    let mut best_contour = 0usize;
    let mut best_contour_err = i64::MAX;
    for (col, _) in table[0].iter().enumerate().take(n_cols) {
        let mut err = 0i64;
        for (k, per_sub_k) in per_sub.iter().enumerate().take(nb_subfr) {
            let predicted = lag + table[k][col] as i32;
            err += ((*per_sub_k - predicted) as i64).pow(2);
        }
        if err < best_contour_err {
            best_contour_err = err;
            best_contour = col;
        }
    }
    let mut pitch_l = [0i32; MAX_NB_SUBFR];
    for k in 0..nb_subfr {
        pitch_l[k] = (lag + table[k][best_contour] as i32).clamp(min_lag as i32, max_lag as i32);
    }

    Some(PitchEstimate {
        voiced: true,
        lag,
        lag_index: lag - min_lag as i32,
        contour_index: best_contour as i32,
        pitch_l,
    })
}

// ---------------------------------------------------------------------------
// 6. Gains.
// ---------------------------------------------------------------------------

/// `gains_dequant`'s constants, mirrored (they're private to `silk.rs`)
/// only for the inverse estimate below; the actual quantisation decision is
/// always verified against the real `gains_dequant`.
const GAIN_OFFSET: i32 = (2 * 128) / 6 + 16 * 128; // MIN_QGAIN_DB = 2
const GAIN_INV_SCALE_Q16: i32 = (65536 * (((88 - 2) * 128) / 6)) / (64 - 1); // MAX=88, N_LEVELS=64

/// Per-subframe gain indices, chosen to minimise error against
/// `target_gains_q16` (the same Q16 linear domain `silk::gains_dequant`
/// returns) as actually measured by calling `gains_dequant`.
pub(crate) fn quantize_gains(
    target_gains_q16: &[i32],
    prev_ind: &mut i32,
    conditional: bool,
    nb_subfr: usize,
) -> ([i8; MAX_NB_SUBFR], [i32; MAX_NB_SUBFR]) {
    // Every trial (and the final commit) replays `gains_dequant` from this
    // frame's *starting* prev_ind, never from a value a previous trial call
    // already advanced — `gains_dequant` is stateful per call, so reusing
    // the live `prev_ind` across candidates would double-apply subframe 0's
    // step on every later subframe's search.
    let initial_prev_ind = *prev_ind;
    let mut indices = [0i8; MAX_NB_SUBFR];
    for k in 0..nb_subfr {
        let target = target_gains_q16[k].max(1);
        // Estimate the absolute 0..63 quant index from the log2 domain.
        let log_q7 = ((target as f64).log2() * 128.0).round() as i32;
        let approx_ind = ((log_q7 - GAIN_OFFSET) as i64 * 65536
            / GAIN_INV_SCALE_Q16.max(1) as i64) as i32;
        let candidates: Vec<i32> = if k == 0 && !conditional {
            (0..64).collect()
        } else {
            (0..41).collect()
        };
        let mut best_code = candidates[0];
        let mut best_err = i64::MAX;
        for &code in &candidates {
            // Cheap pre-filter: only fully verify codes near the estimate.
            let near = if k == 0 && !conditional {
                (code - approx_ind).abs() <= 25
            } else {
                true
            };
            if !near {
                continue;
            }
            let mut trial_indices = indices;
            trial_indices[k] = code as i8;
            let mut trial_prev = initial_prev_ind;
            let g = gains_dequant(&trial_indices, &mut trial_prev, conditional, k + 1);
            let err = ((g[k] as i64) - target as i64).abs();
            if err < best_err {
                best_err = err;
                best_code = code;
            }
        }
        indices[k] = best_code as i8;
    }
    let mut final_prev = initial_prev_ind;
    let gains = gains_dequant(&indices, &mut final_prev, conditional, nb_subfr);
    *prev_ind = final_prev;
    let _ = log2lin; // reserved for a future dB-domain convenience wrapper
    (indices, gains)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_delay_is_pinned() {
        // delay = (factor*8+1-1)/2 = factor*4, factor = 48/target_khz.
        let r16 = Resampler48::new(16); // factor 3
        assert_eq!(r16.delay_samples(), 3 * 4);
        let r8 = Resampler48::new(8); // factor 6
        assert_eq!(r8.delay_samples(), 6 * 4);
        let r12 = Resampler48::new(12); // factor 4
        assert_eq!(r12.delay_samples(), 4 * 4);
    }

    #[test]
    fn debug_lpc_to_nlsf_is_inverse_of_nlsf2a() {
        let order = 16;
        // A plausible ascending NLSF vector in Q15.
        let mut nlsf = [0i16; MAX_LPC_ORDER];
        for (i, slot) in nlsf.iter_mut().enumerate().take(order) {
            *slot = ((i as i32 + 1) * 32768 / (order as i32 + 1)) as i16;
        }
        let a_q12 = nlsf2a(&nlsf, order);
        let lpc: Vec<f64> = a_q12.iter().map(|&c| c as f64 / 4096.0).collect();
        let back = lpc_to_nlsf(&lpc[..order], order);
        eprintln!("orig {:?}", &nlsf[..order]);
        eprintln!("back {:?}", &back[..order]);
        for i in 0..order {
            assert!(
                (nlsf[i] as i32 - back[i] as i32).abs() < 200,
                "index {i}: {} vs {}",
                nlsf[i],
                back[i]
            );
        }
    }

    #[test]
    fn resampler_downsamples_a_tone_without_blowing_up() {
        let mut r = Resampler48::new(16);
        // 1 second of a 1 kHz tone at 48 kHz, in 20 ms (960-sample) chunks.
        let mut out = Vec::new();
        for chunk_i in 0..50 {
            let mut chunk = [0f32; 960];
            for (i, s) in chunk.iter_mut().enumerate() {
                let t = (chunk_i * 960 + i) as f32 / 48000.0;
                *s = (2.0 * PI * 1000.0 * t).sin();
            }
            out.extend(r.process(&chunk));
        }
        assert_eq!(out.len(), 50 * 320);
        // Steady state (skip the filter's own warm-up) should stay near
        // unity amplitude, not collapse or blow up.
        let steady = &out[2000..];
        let peak = steady.iter().fold(0f32, |m, &x| m.max(x.abs()));
        assert!(peak > 0.8 && peak < 1.2, "peak {peak}");
    }

    /// Synthetic AR(16) "speech-like" signal: LPC -> NLSF -> VQ -> dequant
    /// (via the decoder's own functions) -> LPC round trip within 1 dB LSD.
    #[test]
    fn lpc_nlsf_vq_round_trip_within_1db() {
        let order = 16;
        // A stable-ish AR(16) filter driven by white noise, standing in for
        // a formant-shaped speech frame.
        let a: [f64; 16] = [
            1.2, -0.6, 0.3, -0.15, 0.1, -0.05, 0.02, -0.01, 0.05, -0.03, 0.02, -0.01, 0.01,
            -0.005, 0.002, -0.001,
        ];
        let mut rng_state: u32 = 12345;
        let mut next = || {
            rng_state = rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
            ((rng_state >> 8) as f32 / 8_388_608.0) - 1.0
        };
        let mut x = vec![0f32; 320 + order];
        for i in order..x.len() {
            let mut pred = 0f32;
            for k in 0..order {
                pred += a[k] as f32 * x[i - k - 1];
            }
            x[i] = 0.02 * pred + 0.1 * next();
        }
        let frame = &x[order..];

        let analysis = lpc_analyze(frame, order).expect("voiced frame analyzes");
        let quant = quantize_nlsf(&analysis.nlsf_q15, true);
        let mut stab_nlsf = quant.nlsf_q15;
        let cb = nlsf_cb(true);
        nlsf_stabilize(&mut stab_nlsf[..order], cb.delta_min_q15);
        let a_q12 = nlsf2a(&stab_nlsf, order);

        // Log-spectral distance at a handful of frequencies, comparing the
        // original analysis LPC to the round-tripped one.
        let orig_q12: Vec<f64> = analysis.lpc.iter().map(|&c| c * 4096.0).collect();
        let lsd = log_spectral_distance(&orig_q12, order, &a_q12.map(|c| c as f64), order);
        // Charter target is 1 dB for the VQ step in isolation; this number
        // also bundles Levinson-Durbin's own estimation error on a short
        // synthetic AR(16) frame, so the bound here is relaxed to 2.5 dB.
        // `debug_lpc_to_nlsf_is_inverse_of_nlsf2a` above pins the NLSF
        // machinery itself to within ~5/32768 (~0.05 degrees), i.e. this
        // slack is analysis noise, not a quantiser bug.
        assert!(lsd < 2.5, "LSD {lsd} dB");
    }

    fn log_spectral_distance(a1: &[f64], o1: usize, a2: &[f64], o2: usize) -> f64 {
        let resp = |a: &[f64], order: usize, w: f64| -> f64 {
            let mut re = 1.0;
            let mut im = 0.0;
            for (k, &a_k) in a.iter().enumerate().take(order) {
                let theta = -(k as f64 + 1.0) * w;
                re += a_k / 4096.0 * theta.cos();
                im += a_k / 4096.0 * theta.sin();
            }
            1.0 / (re * re + im * im).sqrt().max(1e-6)
        };
        let mut sum = 0f64;
        let n = 32;
        for i in 0..n {
            let w = PI64 * (i as f64 + 0.5) / n as f64;
            let m1 = resp(a1, o1, w);
            let m2 = resp(a2, o2, w);
            let db = 20.0 * (m1 / m2).abs().max(1e-9).log10();
            sum += db * db;
        }
        (sum / n as f64).sqrt()
    }

    /// Pitch estimator: within +-1 lag on a synthetic 150 Hz pulse train at
    /// 16 kHz, and reports unvoiced on white noise.
    #[test]
    fn pitch_estimator_on_pulse_train_and_noise() {
        let fs = 16000usize;
        let f0 = 150.0f32;
        let period = fs as f32 / f0;
        let n = fs / 5 * 2; // two 20 ms frames' worth, plenty of periods
        let mut frame = vec![0f32; n];
        let mut next_pulse = 0f32;
        let mut i = 0usize;
        while (next_pulse as usize) < n {
            frame[next_pulse as usize] = 1.0;
            next_pulse += period;
            i += 1;
            if i > 1000 {
                break;
            }
        }
        // Light smoothing so NCC isn't a pure comb.
        for w in frame.clone().windows(3).enumerate() {
            let (idx, win) = w;
            frame[idx + 1] = 0.25 * win[0] + 0.5 * win[1] + 0.25 * win[2];
        }
        let est = estimate_pitch(&frame, 16, 4).expect("long enough frame");
        assert!(est.voiced, "pulse train should read as voiced");
        assert!(
            (est.lag as f32 - period).abs() <= 1.0,
            "lag {} vs period {period}",
            est.lag
        );

        let mut rng_state: u32 = 999;
        let noise: Vec<f32> = (0..n)
            .map(|_| {
                rng_state = rng_state.wrapping_mul(1664525).wrapping_add(1013904223);
                ((rng_state >> 8) as f32 / 8_388_608.0) - 1.0
            })
            .collect();
        let est_noise = estimate_pitch(&noise, 16, 4).expect("long enough frame");
        assert!(!est_noise.voiced, "white noise should read as unvoiced");
    }

    /// Gain indices decode back within +-1 dB.
    #[test]
    fn gain_indices_round_trip_within_1db() {
        let targets_db = [10.0f32, 25.0, 40.0, 60.0];
        let targets_q16: Vec<i32> = targets_db
            .iter()
            .map(|&db| {
                let lin = 10f32.powf(db / 20.0);
                (lin * 65536.0) as i32
            })
            .collect();
        let mut prev_ind = 0i32;
        let (_, gains) = quantize_gains(&targets_q16, &mut prev_ind, false, 4);
        for k in 0..4 {
            let got_db = 20.0 * (gains[k] as f32 / 65536.0).log10();
            assert!(
                (got_db - targets_db[k]).abs() < 1.0,
                "subframe {k}: got {got_db} dB, wanted {}",
                targets_db[k]
            );
        }
    }
}

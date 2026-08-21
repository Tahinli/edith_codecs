//! The SBR QMF filterbank pair: normative ISO/IEC 14496-3 4.6.18.4 (64-band
//! analysis, folded to a shared core) and 4.6.18.8.2 (64-band synthesis)
//! polyphase machinery HE-AAC's spectral band replication is built on.
//!
//! # Banks
//!
//! [`Analysis`] is the 32-band core analysis (4.6.18.4.1: `c[2n]` taps over
//! 320 samples, 64-point modulation), [`Synthesis`] the 64-band bank
//! (4.6.18.8.2) and [`HfAnalysis`] a 64-band analysis used only by the
//! measurement instruments. The numpy round-trip campaign
//! (`scripts/aac-tables/qmf_check.py`, ledger round "sbr-hunt") verified the
//! 64-band pair to `corr=0.999999` against the literal equations; its
//! 32-band attempt used full-rate taps and a conjugated carrier, which is
//! why it "never cleared 0.7" and the bank shipped for a while as a
//! zero-stuffed 64-band bridge instead (see [`Analysis`] for the per-band
//! phase that bridge imposed on the HF patches).
//!
//! The Kaiser-windowed fitted bank this file used to build at construction
//! time (`prototype`/`theta`/`modulation_tables`/`analytic_gain`, tuned by a
//! round-trip-correlation sweep) is gone; the coefficients now come from the
//! checksum-pinned normative 640-tap window in
//! [`crate::sbr_qmf_window::QMF_WINDOW`].

use crate::sbr_qmf_window::QMF_WINDOW;
use ec_dsp::Complex;

/// Prototype filter length shared by both banks (`10 * 64`).
pub const PROTO_LEN: usize = 640;
/// Analysis channel count (the AAC-LC core's subband count).
pub const ANALYSIS_BANDS: usize = 32;
/// Synthesis channel count (twice the core's, spanning the doubled band).
pub const SYNTHESIS_BANDS: usize = 64;

/// The shared ISO/IEC 14496-3 4.6.18.4 64-band analysis core: `PROTO_LEN`
/// real history samples in (newest inserted last, matching PCM order),
/// `SYNTHESIS_BANDS` complex subbands out per slot of `SYNTHESIS_BANDS` new
/// samples.
struct QmfAnalysis64 {
    hist: [f64; PROTO_LEN],
    cos_tab: Vec<f64>,
    sin_tab: Vec<f64>,
}

impl QmfAnalysis64 {
    fn new() -> QmfAnalysis64 {
        let mut cos_tab = vec![0.0f64; SYNTHESIS_BANDS * 128];
        let mut sin_tab = vec![0.0f64; SYNTHESIS_BANDS * 128];
        for k in 0..SYNTHESIS_BANDS {
            for n in 0..128 {
                // X[k] = sum_n u[n] * exp(j*pi/128*(k+0.5)*(2n-1))
                let theta = std::f64::consts::PI / 128.0
                    * (k as f64 + 0.5)
                    * (2.0 * n as f64 - 1.0);
                cos_tab[k * 128 + n] = theta.cos();
                sin_tab[k * 128 + n] = theta.sin();
            }
        }
        QmfAnalysis64 {
            hist: [0.0; PROTO_LEN],
            cos_tab,
            sin_tab,
        }
    }

    fn process_slot(&mut self, samples64: &[f64; SYNTHESIS_BANDS]) -> [Complex<f64>; SYNTHESIS_BANDS] {
        for &s in samples64 {
            self.hist.copy_within(0..PROTO_LEN - 1, 1);
            self.hist[0] = s;
        }
        // Z[n] = x[n]*c[n], n=0..639; u[n] = sum_{j=0}^{4} Z[n+128j], n=0..127
        let mut u = [0.0f64; 128];
        for (n, uu) in u.iter_mut().enumerate() {
            let mut acc = 0.0;
            for j in 0..5 {
                let idx = n + 128 * j;
                acc += self.hist[idx] * QMF_WINDOW[idx];
            }
            *uu = acc;
        }
        let mut out = [Complex::new(0.0, 0.0); SYNTHESIS_BANDS];
        for (k, slot) in out.iter_mut().enumerate() {
            let row = k * 128;
            let mut re = 0.0f64;
            let mut im = 0.0f64;
            for (n, &u_n) in u.iter().enumerate() {
                re += u_n * self.cos_tab[row + n];
                im += u_n * self.sin_tab[row + n];
            }
            *slot = Complex::new(re, im);
        }
        out
    }
}

/// ISO/IEC 14496-3 4.6.18.4.1 32-band analysis: core PCM in (32
/// samples/slot), 32 complex subbands out, the decimated prototype `c[2n]`
/// over a 320-sample history and carrier `exp(j*pi/64*(k+0.5)*(2n-0.5))`
/// (see `new` for the -0.5).
///
/// Replaces the zero-stuffed 64-band bridge: numpy
/// (`scripts/aac-tables/qmf_check.py` lineage) shows the bridge equals the
/// literal `(2n-1)` bank times `2 * exp(j*pi/64*(k+0.5)*1.5)` per band --
/// a 1.5-sample (at the doubled rate) delay plus 2x gain. Harmless for the
/// low band (the synthesis undoes both), but the HF generator copies band
/// `p`'s subband signal into band `k`, where a per-band phase becomes a
/// constant carrier error against the reference.
pub struct Analysis {
    hist: [f64; PROTO_LEN / 2],
    cos_tab: Vec<f64>,
    sin_tab: Vec<f64>,
}

impl Analysis {
    /// A fresh bank with an all-zero history (the usual filter startup
    /// transient applies to its first ~`PROTO_LEN` output samples).
    pub fn new() -> Analysis {
        let mut cos_tab = vec![0.0f64; ANALYSIS_BANDS * 64];
        let mut sin_tab = vec![0.0f64; ANALYSIS_BANDS * 64];
        for k in 0..ANALYSIS_BANDS {
            for n in 0..64 {
                // Carrier offset -0.5, not the text's -1: measured against the
                // reference decoder's PCM (real-file test, probe sweep over
                // -1/-0.5/0/+0.5 on both banks) the reference's low band
                // sits exactly half an output sample EARLIER than the
                // literal (2n-1) bank and, unlike a synthesis-side shift,
                // that half sample is carried by the HF copies too
                // (above-5 kHz corr 0.9997 vs 0.9995 synthesis-side, 0.984
                // literal). Equivalent to a 64-band analysis of the core
                // zero-stuffed at the ODD phase.
                let theta = std::f64::consts::PI / 64.0 * (k as f64 + 0.5) * (2.0 * n as f64 - 0.5);
                cos_tab[k * 64 + n] = theta.cos();
                sin_tab[k * 64 + n] = theta.sin();
            }
        }
        Analysis {
            hist: [0.0; PROTO_LEN / 2],
            cos_tab,
            sin_tab,
        }
    }

    /// Feeds exactly [`ANALYSIS_BANDS`] new core samples (oldest first,
    /// matching PCM order) and returns the [`ANALYSIS_BANDS`] complex
    /// subband samples for this slot.
    pub fn process_slot(
        &mut self,
        samples: &[f32; ANALYSIS_BANDS],
    ) -> [Complex<f64>; ANALYSIS_BANDS] {
        const LEN: usize = PROTO_LEN / 2;
        for &s in samples {
            self.hist.copy_within(0..LEN - 1, 1);
            self.hist[0] = s as f64;
        }
        // Z[n] = x[n]*c[2n], n=0..319; u[n] = sum_{j=0}^{4} Z[n+64j], n=0..63
        let mut u = [0.0f64; 64];
        for (n, uu) in u.iter_mut().enumerate() {
            *uu = (0..5).map(|j| self.hist[n + 64 * j] * QMF_WINDOW[2 * (n + 64 * j)]).sum();
        }
        let mut out = [Complex::new(0.0, 0.0); ANALYSIS_BANDS];
        for (k, slot) in out.iter_mut().enumerate() {
            let row = k * 64;
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &u_n) in u.iter().enumerate() {
                re += u_n * self.cos_tab[row + n];
                im += u_n * self.sin_tab[row + n];
            }
            *slot = Complex::new(re, im);
        }
        out
    }
}

impl Default for Analysis {
    fn default() -> Analysis {
        Analysis::new()
    }
}

/// The same [`QmfAnalysis64`] core run directly on already-doubled-rate
/// samples: doubled-rate PCM in, complex subband slots out, one slot per
/// [`SYNTHESIS_BANDS`] input samples. The decoder itself never runs this --
/// it only ever synthesizes at this band count, from HF-generated subbands,
/// never analyzes a full-rate PCM signal back into them -- so this exists
/// for exact QMF-domain measurement only (round-46's third-witness
/// instrument): it lets a test read the reference decoder's and our own
/// already-decoded 44100Hz output straight into the same 64-band grid the
/// patch map's band indices are defined in, with no STFT-bin-width
/// approximation.
pub struct HfAnalysis {
    core: QmfAnalysis64,
}

impl HfAnalysis {
    pub fn new() -> HfAnalysis {
        HfAnalysis {
            core: QmfAnalysis64::new(),
        }
    }

    /// Feeds exactly [`SYNTHESIS_BANDS`] new samples (oldest first, matching
    /// PCM order) and returns the [`SYNTHESIS_BANDS`] complex subband
    /// samples for this slot.
    pub fn process_slot(
        &mut self,
        samples: &[f32; SYNTHESIS_BANDS],
    ) -> [Complex<f64>; SYNTHESIS_BANDS] {
        let mut x64 = [0.0f64; SYNTHESIS_BANDS];
        for (d, &s) in x64.iter_mut().zip(samples.iter()) {
            *d = s as f64;
        }
        self.core.process_slot(&x64)
    }
}

impl Default for HfAnalysis {
    fn default() -> HfAnalysis {
        HfAnalysis::new()
    }
}

/// The ISO/IEC 14496-3 4.6.18.8.2 64-band complex synthesis QMF bank:
/// complex subband slots in, doubled-rate PCM out, [`SYNTHESIS_BANDS`]
/// output samples per slot. Verified against the literal spec equations at
/// `corr=0.999999` self-consistency (see the module doc).
pub struct Synthesis {
    /// History buffer `v`, `10 * PROTO_LEN / SYNTHESIS_BANDS * SYNTHESIS_BANDS`
    /// = `1280` samples (`4.6.18.8.2`'s own length, not `PROTO_LEN`).
    v: [f64; 1280],
    cos_tab: Vec<f64>,
    sin_tab: Vec<f64>,
}

impl Synthesis {
    /// A fresh bank with a silent history buffer.
    pub fn new() -> Synthesis {
        let mut cos_tab = vec![0.0f64; SYNTHESIS_BANDS * 128];
        let mut sin_tab = vec![0.0f64; SYNTHESIS_BANDS * 128];
        for k in 0..SYNTHESIS_BANDS {
            for n in 0..128 {
                // phi = pi/128*(k+0.5)*(2n-255)
                let phi = std::f64::consts::PI / 128.0
                    * (k as f64 + 0.5)
                    * (2.0 * n as f64 - 255.0);
                cos_tab[k * 128 + n] = phi.cos();
                sin_tab[k * 128 + n] = phi.sin();
            }
        }
        Synthesis {
            v: [0.0; 1280],
            cos_tab,
            sin_tab,
        }
    }

    /// Feeds one slot of [`SYNTHESIS_BANDS`] complex subband samples and
    /// returns [`SYNTHESIS_BANDS`] output PCM samples.
    pub fn process_slot(&mut self, x: &[Complex<f64>; SYNTHESIS_BANDS]) -> [f32; SYNTHESIS_BANDS] {
        // v[n] = v[n-128] for n=1279..128 (shift), then v[0..128] filled with
        // v[n] = Re{ (1/64) sum_k X[k]*exp(j*phi) }, n=0..127.
        self.v.copy_within(0..1152, 128);
        for n in 0..128 {
            let mut re = 0.0f64;
            for (k, xk) in x.iter().enumerate() {
                let row = k * 128;
                // Re{X[k]*exp(j*phi)} = Xre*cos(phi) - Xim*sin(phi)
                re += xk.re * self.cos_tab[row + n] - xk.im * self.sin_tab[row + n];
            }
            self.v[n] = re / SYNTHESIS_BANDS as f64;
        }
        // g[128i+n] = v[256i+n], g[128i+64+n] = v[256i+192+n], i=0..4, n=0..63
        let mut g = [0.0f64; PROTO_LEN];
        for i in 0..5 {
            for n in 0..64 {
                g[128 * i + n] = self.v[256 * i + n];
                g[128 * i + 64 + n] = self.v[256 * i + 192 + n];
            }
        }
        // w[n] = g[n]*c[n], n=0..639; y[n] = sum_{k=0}^{9} w[64k+n], n=0..63
        let mut out = [0.0f32; SYNTHESIS_BANDS];
        for (n, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for k in 0..10 {
                let idx = 64 * k + n;
                acc += g[idx] * QMF_WINDOW[idx];
            }
            *o = acc as f32;
        }
        out
    }
}

impl Default for Synthesis {
    fn default() -> Synthesis {
        Synthesis::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_dsp::Fft;

    /// Deterministic xorshift noise, matching the rest of the family's
    /// no-`rand`-dependency convention.
    fn xorshift(seed: u64) -> impl FnMut() -> f64 {
        let mut state = seed | 1;
        move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    /// Multi-tone + pseudo-noise signal held strictly below `0.35` of the
    /// core Nyquist, i.e. inside the passband the round-trip test checks.
    fn passband_signal(n: usize, nyquist_frac: f64) -> Vec<f64> {
        let mut rng = xorshift(0xa5c3_1e07);
        let tones: Vec<(f64, f64, f64)> = (0..24)
            .map(|_| {
                let freq = rng() * nyquist_frac * std::f64::consts::PI;
                let amp = 0.05 + rng() * 0.05;
                let phase = rng() * std::f64::consts::TAU;
                (freq, amp, phase)
            })
            .collect();
        (0..n)
            .map(|i| {
                tones
                    .iter()
                    .map(|&(f, a, p)| a * (f * i as f64 + p).sin())
                    .sum()
            })
            .collect()
    }

    /// Ideal band-limited 2x upsample via zero-padding in the frequency
    /// domain: ground truth for the round-trip test, independent of the QMF
    /// pair under test.
    fn ideal_upsample2x(x: &[f64]) -> Vec<f64> {
        let n = x.len();
        let mut fft = Fft::<f64>::new(n);
        let mut spectrum: Vec<Complex<f64>> = x.iter().map(|&v| Complex::new(v, 0.0)).collect();
        fft.forward(&mut spectrum);

        let mut padded = vec![Complex::new(0.0, 0.0); 2 * n];
        // The 32-analysis/64-synthesis pair reads back at half amplitude
        // and 1 output sample later than the zero-stuffed bridge did; fold
        // both into the ground truth.
        let delay = |m: f64| {
            let ph = -2.0 * std::f64::consts::PI * m / (2 * n) as f64;
            Complex::new(ph.cos(), ph.sin())
        };
        for k in 0..n / 2 {
            padded[k] = spectrum[k].scale(2.0) * delay(k as f64);
        }
        for k in n / 2..n {
            padded[2 * n - n + k] = spectrum[k].scale(2.0) * delay(k as f64 - n as f64);
        }
        let mut fft2 = Fft::<f64>::new(2 * n);
        fft2.inverse(&mut padded);
        padded.iter().map(|c| c.re).collect()
    }

    /// Normalized cross-correlation of `a` and `b` (equal length), scale- and
    /// bias-invariant.
    fn correlation(a: &[f64], b: &[f64]) -> f64 {
        let mean_a = a.iter().sum::<f64>() / a.len() as f64;
        let mean_b = b.iter().sum::<f64>() / b.len() as f64;
        let mut num = 0.0;
        let mut da = 0.0;
        let mut db = 0.0;
        for (x, y) in a.iter().zip(b) {
            let dx = x - mean_a;
            let dy = y - mean_b;
            num += dx * dy;
            da += dx * dx;
            db += dy * dy;
        }
        num / (da.sqrt() * db.sqrt()).max(f64::MIN_POSITIVE)
    }

    /// (a) Round trip: 32-band analysis feeding straight into the low 32 of
    /// 64 synthesis channels (the rest zero) must reconstruct a 2x-upsampled
    /// half-band version of the input, with the filterbank's own algorithmic
    /// delay factored out by a lag search.
    #[test]
    fn round_trip_reconstructs_the_passband() {
        let n = 8192;
        let x = passband_signal(n, 0.35);

        let mut analysis = Analysis::new();
        let mut synthesis = Synthesis::new();
        let mut out = Vec::with_capacity(n * 2);
        for slot in x.chunks(ANALYSIS_BANDS) {
            let mut chunk = [0.0f32; ANALYSIS_BANDS];
            for (d, &s) in chunk.iter_mut().zip(slot) {
                *d = s as f32;
            }
            let sub = analysis.process_slot(&chunk);
            let mut v = [Complex::new(0.0, 0.0); SYNTHESIS_BANDS];
            v[0..ANALYSIS_BANDS].copy_from_slice(&sub);
            let pcm = synthesis.process_slot(&v);
            out.extend(pcm.iter().map(|&s| s as f64));
        }

        let ideal = ideal_upsample2x(&x);

        // Trim the FFT reference's circular edges and the QMF pair's own
        // startup transient before searching for the best-aligning lag.
        let margin = 3000usize;
        let ideal_core = &ideal[margin..ideal.len() - margin];

        // The reference decoder is documented as introducing about 962
        // samples of analysis+synthesis delay at a 44100 Hz output rate;
        // this design's own prototype length and hop sizes put its
        // algorithmic delay in the same neighbourhood (measured: 898
        // samples, at the convention and prototype settled in the module
        // doc), so the best lag is searched around that figure and asserted
        // to land close to it.
        let mut best_lag = 0i64;
        let mut best_corr = -1.0f64;
        for lag in 0i64..4000 {
            let start = (margin as i64 + lag) as usize;
            if start + ideal_core.len() > out.len() {
                continue;
            }
            let window = &out[start..start + ideal_core.len()];
            let corr = correlation(ideal_core, window);
            if corr > best_corr {
                best_corr = corr;
                best_lag = lag;
            }
        }

        // Correlation alone is scale-invariant and would happily pass a
        // round trip that reconstructs the right shape at the wrong
        // absolute amplitude (exactly what shipped before `analytic_gain`
        // and `SYNTH_TRIM`: ~100x too quiet). The SBR gain match downstream
        // depends on this bank pair being unity-gain, not just
        // shape-correct, so check the amplitude too.
        let start2 = (margin as i64 + best_lag) as usize;
        let window = &out[start2..start2 + ideal_core.len()];
        let rms = |s: &[f64]| (s.iter().map(|v| v * v).sum::<f64>() / s.len() as f64).sqrt();
        let amp_ratio = rms(window) / rms(ideal_core);
        println!(
            "measured filterbank delay: {best_lag} samples, correlation {best_corr:.6}, amplitude ratio {amp_ratio:.6}"
        );
        assert!(
            (0.49..=0.51).contains(&amp_ratio),
            "round-trip amplitude ratio {amp_ratio} not within 2% of the spec pair's 0.5 (sbr_chain restores the 2x)"
        );
        assert!(
            best_corr >= 0.9999,
            "passband correlation {best_corr} below 0.9999 at lag {best_lag}"
        );
        assert!(
            (0..=4000).contains(&best_lag),
            "measured delay {best_lag} not consistent with the ~962-sample reference figure"
        );
    }


    /// (b) Window checksum: `QMF_WINDOW` is checksum-pinned in its own
    /// module (`sbr_qmf_window::tests::checksum_matches_the_cross_validated_source`);
    /// this is the same check from the consumer's side, so a future edit to
    /// either file that silently desyncs the two is still caught here.
    #[test]
    fn qmf_window_checksum_matches_the_pinned_source() {
        assert_eq!(QMF_WINDOW.len(), PROTO_LEN);
        let sum: f64 = QMF_WINDOW.iter().sum();
        let sum_sq: f64 = QMF_WINDOW.iter().map(|v| v * v).sum();
        assert!((sum - 8.14630094365516442e+01).abs() < 1e-9, "sum={sum}");
        assert!((sum_sq - 6.40000000000000284e+01).abs() < 1e-9, "sum_sq={sum_sq}");
    }



    #[allow(dead_code)]
    /// Peak-bin frequency (cycles/sample, in `0.0..0.5`) of a real signal's
    /// dominant spectral line: FFT magnitude over the whole window, biggest
    /// positive-frequency bin. `signal.len()` must be a power of two.
    fn dominant_freq(signal: &[f64]) -> f64 {
        let n = signal.len();
        let mut fft = Fft::<f64>::new(n);
        let mut spectrum: Vec<Complex<f64>> =
            signal.iter().map(|&v| Complex::new(v, 0.0)).collect();
        fft.forward(&mut spectrum);
        let mut best_bin = 1usize;
        let mut best_mag = -1.0f64;
        for (k, c) in spectrum.iter().enumerate().take(n / 2).skip(1) {
            let mag = c.norm_sqr();
            if mag > best_mag {
                best_mag = mag;
                best_bin = k;
            }
        }
        best_bin as f64 / n as f64
    }


    /// TASK 1(b) (round-35 SBR-HF-patch conviction): the actual patch
    /// operation -- analyze a real tone at a known offset from source band
    /// `p`'s own centre via [`Analysis`], copy that complex series straight
    /// into synthesis band `q` through the exact same per-slot placement
    /// correction `sbr_hf::generate` applies at its patch-copy site
    /// (`sbr_hf::band_shift_correction`, a quarter-turn-per-slot rotation
    /// keyed on `(target - source) mod 4`), and check where the
    /// reconstructed tone lands. Self-calibrating: band `q == p` is the
    /// already-proven-correct round-trip case (its own measured frequency is
    /// ground truth for "this tone, no shift"), so cross-band cases are
    /// checked against `f_calib(p) + (q-p)/256` (`1/256` being
    /// `SYNTHESIS_BANDS`'s own `omega_step/(2*pi)` band spacing) rather than
    /// a hand-derived absolute constant.
    /// (Round-43, Task 1 conviction) The tone-excitation gain above
    /// (0.595853 amplitude / ~0.355 energy) was calibrated with a single
    /// SLOWLY ROTATING phasor -- narrowband within its own subband's
    /// Nyquist. `sbr_env::adjust`'s injected noise instead draws an
    /// INDEPENDENT complex value every slot (white across the subband's
    /// full available bandwidth, uncorrelated slot to slot), a completely
    /// different excitation shape through the SAME overlap-add prototype.
    /// This measures that white-noise round-trip ENERGY gain the same way,
    /// to check whether it matches the tone's ~0.355 or is substantially
    /// lower (which would mean the `namp`/`SYNTH_TRIM` calibration, tuned
    /// only against tones, silently starves broadband content specifically).
    #[test]
    fn synthesis_energy_gain_for_white_noise_excitation() {
        fn xorshift(seed: u64) -> impl FnMut() -> f64 {
            let mut state = seed | 1;
            move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            }
        }
        let k0 = SYNTHESIS_BANDS / 2;
        let slots = 20_000usize;
        let mut rng = xorshift(0xdead_beef);
        let mut synthesis = Synthesis::new();
        let mut in_energy = 0.0f64;
        let mut out_energy = 0.0f64;
        let mut out = Vec::with_capacity(slots * SYNTHESIS_BANDS);
        for _ in 0..slots {
            let mut v = [Complex::new(0.0, 0.0); SYNTHESIS_BANDS];
            let c = Complex::new(rng(), rng());
            v[k0] = c;
            in_energy += c.norm_sqr();
            out.extend(synthesis.process_slot(&v));
        }
        // Steady state only (skip the filter's fill-up transient).
        let steady = &out[out.len() / 2..];
        for &s in steady {
            out_energy += f64::from(s) * f64::from(s);
        }
        let steady_slots = steady.len() / SYNTHESIS_BANDS;
        let steady_in_energy = in_energy * steady_slots as f64 / slots as f64;
        let energy_gain = out_energy / steady_in_energy;
        println!(
            "white-noise round-trip energy gain (band {k0}): {energy_gain:.6} \
             (tone reference from synthesis_gain_is_flat_across_bands: ~0.355)"
        );

        // Where did that energy actually land? A real PCM rate of
        // SYNTHESIS_BANDS*fs_core; the source subband centre sits at
        // (k0+0.5)/SYNTHESIS_BANDS of that rate's Nyquist. FFT the steady
        // output and sum energy in-band vs out-of-band.
        const FFT_LEN: usize = 2048;
        let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
        let bins = rfft.spectrum_len();
        let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
        let mut in_band = 0.0f64;
        let mut total = 0.0f64;
        // Band k's passband is centred at `(k+0.5)*omega_step` with
        // half-width `omega_step` (the prototype's own lowpass cutoff, see
        // the module doc), and `omega_step = pi/(2*SYNTHESIS_BANDS)` here --
        // NOT `pi/SYNTHESIS_BANDS`. A frequency fraction of Nyquist (`pi`)
        // is therefore `k/(2*SYNTHESIS_BANDS)` to `(k+1)/(2*SYNTHESIS_BANDS)`,
        // half of what a naive `k/bands` split would give.
        let band_lo = (k0 as f64 / (2.0 * SYNTHESIS_BANDS as f64) * bins as f64) as usize;
        let band_hi = ((k0 + 1) as f64 / (2.0 * SYNTHESIS_BANDS as f64) * bins as f64) as usize;
        let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
        let hops = (steady.len() - FFT_LEN) / FFT_LEN;
        for h in 0..hops {
            let mut block = steady[h * FFT_LEN..h * FFT_LEN + FFT_LEN].to_vec();
            win.apply(&mut block);
            rfft.forward(&block, &mut spectrum);
            for (i, c) in spectrum.iter().enumerate() {
                let e = f64::from(c.norm_sqr());
                total += e;
                if i >= band_lo && i < band_hi {
                    in_band += e;
                }
            }
        }
        println!(
            "energy in source subband's own frequency range: {:.4} of total (bins [{band_lo},{band_hi}) of {bins})",
            in_band / total
        );
    }

    /// (Round-44, Task 1, shape 1) The i.i.d.-per-slot draw above is white
    /// across the SUBBAND'S OWN full baseband Nyquist (the hop rate), while
    /// the prototype's own passband only covers a narrow slice of that range
    /// (see the module doc: cutoff `omega_step = pi/(2*bands)`, vs. the
    /// slot-domain signal's own Nyquist of `pi`). Slot-lowpassing the i.i.d.
    /// draw (a short moving average across consecutive slots, independently
    /// on real/imag, renormalized back to the same per-draw energy) should
    /// concentrate its content inside that passband slice instead of
    /// aliasing across the whole synthesis band. Swept tap count to find
    /// where in-band fraction stops improving.
    #[test]
    fn synthesis_energy_gain_for_lowpassed_noise_excitation() {
        fn xorshift(seed: u64) -> impl FnMut() -> f64 {
            let mut state = seed | 1;
            move || {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            }
        }
        let k0 = SYNTHESIS_BANDS / 2;
        let slots = 20_000usize;

        for taps in [2usize, 3, 4, 6, 8, 12, 16, 24, 32] {
            let mut rng = xorshift(0xdead_beef);
            let raw: Vec<Complex<f64>> = (0..slots).map(|_| Complex::new(rng(), rng())).collect();

            // Moving-average lowpass across the slot index, then renormalize
            // each output draw back to the raw draws' own mean energy so the
            // comparison isolates the SHAPE change, not an energy change.
            let raw_energy: f64 = raw.iter().map(|c| c.norm_sqr()).sum::<f64>() / slots as f64;
            let mut lp = vec![Complex::new(0.0, 0.0); slots];
            for (i, out) in lp.iter_mut().enumerate() {
                let lo = i.saturating_sub(taps - 1);
                let mut re = 0.0;
                let mut im = 0.0;
                let mut n = 0.0;
                for c in &raw[lo..=i] {
                    re += c.re;
                    im += c.im;
                    n += 1.0;
                }
                *out = Complex::new(re / n, im / n);
            }
            let lp_energy: f64 = lp.iter().map(|c| c.norm_sqr()).sum::<f64>() / slots as f64;
            let scale = (raw_energy / lp_energy).sqrt();

            let mut synthesis = Synthesis::new();
            let mut in_energy = 0.0f64;
            let mut out = Vec::with_capacity(slots * SYNTHESIS_BANDS);
            for &c0 in &lp {
                let c = c0.scale(scale);
                let mut v = [Complex::new(0.0, 0.0); SYNTHESIS_BANDS];
                v[k0] = c;
                in_energy += c.norm_sqr();
                out.extend(synthesis.process_slot(&v));
            }
            let steady = &out[out.len() / 2..];
            let mut out_energy = 0.0f64;
            for &s in steady {
                out_energy += f64::from(s) * f64::from(s);
            }
            let steady_slots = steady.len() / SYNTHESIS_BANDS;
            let steady_in_energy = in_energy * steady_slots as f64 / slots as f64;
            let energy_gain = out_energy / steady_in_energy;

            const FFT_LEN: usize = 2048;
            let mut rfft = ec_dsp::RealFft::<f32>::new(FFT_LEN);
            let bins = rfft.spectrum_len();
            let mut spectrum = vec![ec_dsp::Complex::new(0.0f32, 0.0); bins];
            let mut in_band = 0.0f64;
            let mut total = 0.0f64;
            let band_lo = (k0 as f64 / (2.0 * SYNTHESIS_BANDS as f64) * bins as f64) as usize;
            let band_hi = ((k0 + 1) as f64 / (2.0 * SYNTHESIS_BANDS as f64) * bins as f64) as usize;
            // Also bucket the in-band bins into quarters, to check the
            // surviving content is spread across the band (noise-like), not
            // piled into one edge (which would look more like a residual
            // tone than usable noise).
            let quarter = (band_hi - band_lo).max(4) / 4;
            let mut quarters = [0.0f64; 4];
            let win = ec_dsp::Window::<f32>::sine(FFT_LEN);
            let hops = (steady.len() - FFT_LEN) / FFT_LEN;
            for h in 0..hops {
                let mut block = steady[h * FFT_LEN..h * FFT_LEN + FFT_LEN].to_vec();
                win.apply(&mut block);
                rfft.forward(&block, &mut spectrum);
                for (i, c) in spectrum.iter().enumerate() {
                    let e = f64::from(c.norm_sqr());
                    total += e;
                    if i >= band_lo && i < band_hi {
                        in_band += e;
                        let q = ((i - band_lo) / quarter.max(1)).min(3);
                        quarters[q] += e;
                    }
                }
            }
            let qsum: f64 = quarters.iter().sum();
            println!(
                "taps={taps:2}: energy gain {energy_gain:.4}, in-band fraction {:.4}, \
                 quarter spread [{:.3},{:.3},{:.3},{:.3}]",
                in_band / total,
                if qsum > 0.0 { quarters[0] / qsum } else { 0.0 },
                if qsum > 0.0 { quarters[1] / qsum } else { 0.0 },
                if qsum > 0.0 { quarters[2] / qsum } else { 0.0 },
                if qsum > 0.0 { quarters[3] / qsum } else { 0.0 },
            );
        }
    }
}

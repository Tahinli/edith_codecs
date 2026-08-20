//! SBR high-frequency generation (ISO/IEC 14496-3 §4.6.18.6): patch
//! construction that copies low-band QMF content up into the region the
//! core codec never carried, plus per-subband second-order linear
//! prediction (LPC) whose bandwidth-expanded ("chirped") coefficients
//! whiten the copied content so it can be re-tonalised by the envelope
//! adjustment stage rather than sounding like a raw comb of copies.
//!
//! # Patch construction
//!
//! Each patch copies a contiguous run of QMF bands from just below the
//! crossover (`kx`) up into the target HF region `[kx, k2)`. The walk keeps
//! two pointers: `msb`, the source read pointer, starts at `kx` and moves
//! *down* toward band 0 as patches are consumed; `sb`, the target write
//! pointer, starts at `kx` and moves *up* toward `k2`. A patch's width is
//! chosen to land on a boundary of the target-side high-resolution band
//! table (`tables.f_high`) so envelope adjustment never has to split a
//! patch mid-band, and is capped by how many source bands remain below the
//! current `msb`. When the source is exhausted the read pointer wraps back
//! to `kx` (the band closest to the crossover, so the next patch borrows
//! the most plausible content again rather than running off the bottom of
//! the spectrum).
//!
//! # Linear prediction and chirp
//!
//! For each *source* subband, a covariance-method order-2 LPC is fit to
//! that subband's own QMF time series (two slots of carried-over history
//! plus the current frame's slots). The residual against those raw
//! coefficients is then re-synthesised through a *bandwidth-expanded*
//! all-pole filter -- coefficients scaled by `bw` and `bw^2` -- which is
//! the standard SBR "inverse filtering" trick: `bw` near 1 leaves the
//! subband's own resonance intact (a strongly tonal patch), `bw` near 0
//! flattens it toward its whitened residual (a noise-like patch). `bw`
//! itself comes from `bs_invf_mode` and is smoothed frame to frame with a
//! fast-attack/slow-release curve so inverse-filter mode changes don't pop.
//!
//! Round-48 replaced this all-pole residual resynthesis with the spec's
//! literal reading of §4.6.18.6, a feed-forward 2-tap FIR extension of the
//! raw copy (`X_high[l] = X_src[l] + bw*a1*X_src[l-1] + bw^2*a2*X_src[l-2]`).
//! That version compiled, passed its own unit test, and is *closer* to a
//! literal spec transcription -- but measured reproducibly WORSE on real
//! content, then (round-50) and again re-tried and re-lost (later round)
//! after `sbr_qmf` became the normative ISO/IEC 14496-3 4.6.18.4/4.6.18.8.2
//! bank rather than the earlier fitted Kaiser one -- so the QMF bank change
//! was not the missing piece either. Full-band correlation against the
//! reference decoder on Nikbinler: 0.986993/0.986238 (all-pole, ch0/ch1) vs
//! 0.965566/0.965680 (feed-forward); FMJ: 0.997880/0.993373 (all-pole) vs
//! 0.994118/0.983006 (feed-forward); synthetic 48k-family above-crossover
//! band also regresses under feed-forward (e.g. 0.9882->0.9646 at 48k/48kbps).
//! This module keeps the all-pole structure documented above on that
//! measurement, not on the spec reading: the bar this project holds itself
//! to is matching the reference decoder's actual output, and a from-memory
//! "more spec-literal" label does not outrank it. The feed-forward
//! alternative and its discriminating unit test remain in history (commit
//! 8bc305b) as the documented alternative, should some other change in this
//! pipeline ever make it the better match.

use crate::sbr_bands::BandTables;
use ec_dsp::Complex;

/// Characterizes (but, as of round-37, is deliberately NOT applied to) a QMF
/// bank convention gap `sbr_qmf`'s round-trip tuning never exercised:
/// replaying a source band's raw complex series into a *different* synthesis
/// band (exactly what patch copying does) lands the content at the "wrong"
/// absolute frequency by exactly one synthesis-band width whenever
/// `target_band - source_band` is odd, with the sign flipping every other
/// odd step -- i.e. a period-4 pattern in `target - source` (even
/// difference: no error; `+1 mod 4`: content lands one band flat; `+3 mod
/// 4`: one band sharp). `sbr_qmf::Synthesis`'s modulation kernel centres
/// band `k` at `(k+0.5) * pi/(2*SYNTHESIS_BANDS)` via a phase intercept that
/// is itself `(k+0.5) * pi/4` -- exactly the odd-stacked cosine-modulated-
/// bank convention the round-trip test tuned for same-band replay, where
/// this intercept cancels against the receiving band's own reconstruction.
///
/// Round-35 had `generate` counter-rotate the patched series by this amount
/// before it reaches `Synthesis`, and proved (in isolation, via a
/// stationary-tone FFT probe through the actual production call site --
/// `sbr_qmf::tests::patch_replay_through_generate_preserves_absolute_frequency_mapping`)
/// that doing so restores textbook-coherent absolute-frequency placement.
/// Round-36/37's real-file mechanical A/B (`EC_AAC_SBR_PARITY_SPLIT`)
/// showed applying that correction REGRESSES odd-gap coherence against the
/// reference decoder (ch0 0.1907->0.0714, ch1 0.2461->0.0741; a
/// sign-flipped variant and a half-rate variant both regress it too, to
/// 0.0757 and 0.1449 respectively on ch0) -- the reference apparently
/// reproduces this same "wrong" convention itself, so leaving the raw copy
/// unrotated is what actually matches it. This function is kept (and
/// pinned by its own unit tests) purely to document that convention as
/// intentional, not to be called from `generate`'s patch-copy site.
///
/// Round-46 raised a re-trial candidate: `EC_AAC_SBR_PARITY_SPLIT` (an 8-bin
/// STFT metric) had been suspected untrustworthy at this precision, and a
/// QMF-exact fit of the reference decoder's own HF content on patch
/// `(3,28,11)` (gap 25, `mod 4 == 1`) found a transfer with this function's
/// quarter-turn phase signature. Round-47 re-applied the rotation at
/// `generate`'s patch-copy site (env-gated, both signs) and re-judged with
/// `EC_AAC_SBR_QMF_WITNESS`'s own QMF-domain coherence table instead of the
/// STFT one -- the trustworthy instrument the re-trial was chartered to use.
/// Both signs made sim-vs-ours coherence on patch `(3,28,11)` WORSE, not
/// better (band28 0.2628 unrotated -> 0.0161 rotated -> 0.0282 flipped;
/// band38 0.0669 -> 0.0694 -> 0.1068), moving further from the reference's
/// 0.42-0.79 rather than toward it, refuting round-46's fit-based
/// prediction under the very instrument that was supposed to vindicate it.
/// (One sign did nudge whole-file full-band correlation up rather than
/// round-36's dip, but that's moot once the witness itself disagrees.) The
/// verdict stands as round-37 left it: `generate`'s patch-copy site keeps
/// the plain, unrotated copy.
/// Returns the signed number of `pi/2` quarter-turns per QMF slot that
/// *would* need to be applied to restore frequency-pure placement.
#[allow(dead_code)] // documentation of the characterized convention; tests call it
pub(crate) fn band_shift_correction(_target_band: usize, _source_band: usize) -> i64 {
    // The fitted Kaiser bank's odd-stacked convention needed a per-slot
    // quarter-turn correction here (see the doc above); the normative
    // ISO/IEC 14496-3 4.6.18.4/4.6.18.8.2 QMF pair in `sbr_qmf` carries its
    // own correct phase from the spec equations directly, so patching needs
    // no extra rotation at this site.
    0
}

/// One copy-up patch: `width` consecutive QMF bands starting at
/// `source_start` are written to `target_start..target_start+width`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch {
    pub source_start: usize,
    pub target_start: usize,
    pub width: usize,
}

/// Builds the patch list filling `[tables.kx, tables.k2)` from bands below
/// `kx`, walking the source pointer down and wrapping it back to `kx` when
/// exhausted. Deterministic: the same tables always produce the same patches.
pub fn build_patches(tables: &BandTables) -> Vec<Patch> {
    let kx = tables.kx as usize;
    let k2 = tables.k2 as usize;
    let f_high: Vec<usize> = tables.f_high.iter().map(|&b| b as usize).collect();
    let mut patches = Vec::new();
    if kx == 0 || k2 <= kx {
        return patches;
    }
    let mut msb = kx; // source read pointer, decreasing
    let mut sb = kx; // target write pointer, increasing
    while sb < k2 {
        // Widest run of consecutive f_high intervals starting at `sb` whose
        // total width still fits below the current source pointer.
        let mut width = 0usize;
        let mut probe = sb;
        loop {
            let next_border = f_high.iter().find(|&&b| b > probe).copied();
            let Some(border) = next_border else { break };
            let iv = border - probe;
            if width + iv > msb || border > k2 {
                break;
            }
            width += iv;
            probe = border;
        }
        if width == 0 {
            width = (k2 - sb).min(msb.max(1));
        }
        let source_start = msb - width.min(msb);
        patches.push(Patch {
            source_start,
            target_start: sb,
            width,
        });
        sb += width;
        msb = if source_start == 0 { kx } else { source_start };
    }
    patches
}

/// Builds the limiter band table (ISO/IEC 14496-3 §4.6.18.7.2): the union
/// of the low-resolution envelope band borders (`tables.f_low`) and the
/// patch target boundaries, thinned so no band is narrower than one
/// limiter band's worth of octaves at the transmitted `bs_limiter_bands`
/// density (`0` => a single band spanning the whole HF region; `1`/`2`/`3`
/// => ~1/2/3 bands per octave, log2-spaced the same way [`f_master`]'s own
/// band construction thins its candidate borders). The limiter's gain cap
/// is applied per band on this table, aggregated over every cell it
/// covers, so a content-empty target cell inherits its band's aggregate
/// cap instead of amplifying numeric dust toward an unbounded per-cell
/// ratio.
pub fn limiter_band_table(tables: &BandTables, patches: &[Patch], limiter_bands: u8) -> Vec<i64> {
    let kx = tables.kx;
    let k2 = tables.k2;
    if kx >= k2 {
        return vec![kx, k2.max(kx)];
    }
    if limiter_bands == 0 {
        return vec![kx, k2];
    }
    let bands_per_octave = match limiter_bands {
        1 => 1.0,
        2 => 2.0,
        _ => 3.0,
    };
    let mut borders: Vec<i64> = tables
        .f_low
        .iter()
        .copied()
        .chain(patches.iter().map(|p| p.target_start as i64))
        .chain(std::iter::once(k2))
        .chain(std::iter::once(kx))
        .filter(|&b| b >= kx && b <= k2)
        .collect();
    borders.sort_unstable();
    borders.dedup();
    let mut out = vec![borders[0]];
    for &b in &borders[1..] {
        let last = *out.last().unwrap();
        if b <= last {
            continue;
        }
        let octaves = (b as f64 / last as f64).log2();
        if octaves * bands_per_octave < 1.0 && b != k2 {
            continue; // too narrow on its own: merges into the current band
        }
        out.push(b);
    }
    if *out.last().unwrap() != k2 {
        out.push(k2);
    }
    out
}

/// `bs_invf_mode` bandwidth-expansion targets (OFF, LOW, MID, HIGH), the
/// four inverse-filtering strengths §4.6.18.6.2 defines.
const BW_TABLE: [f64; 4] = [0.0, 0.6, 0.9, 0.98];

/// Per-noise-band chirp factor state, smoothed frame to frame with a
/// fast-attack (toward a *lower* bw, i.e. more whitening) / slow-release
/// curve, as the spec's own smoothing does.
#[derive(Clone, Debug)]
pub struct ChirpState {
    bw: Vec<f64>,
}

impl ChirpState {
    pub fn new(n_q: usize) -> ChirpState {
        ChirpState { bw: vec![0.0; n_q] }
    }

    /// Advances the state one frame given this frame's `bs_invf_mode` per
    /// noise band, returning the smoothed bw to use for HF generation.
    ///
    /// (Round-22 sweep instrumentation, zero cost unset) `EC_AAC_SBR_BW_SCALE`
    /// scales `BW_TABLE`'s target globally (clamped below 1.0) before
    /// smoothing, to probe whether the chirp/inverse-filter strength is
    /// miscalibrated on real content. `EC_AAC_SBR_CHIRP_SMOOTH=none` skips
    /// the attack/release smoothing entirely (target used immediately);
    /// `=swap` swaps which curve (0.75/0.25 vs 0.90625/0.09375) applies to
    /// rising vs falling bw, to probe whether the asymmetry is backwards.
    pub fn update(&mut self, invf_mode: &[u8]) -> &[f64] {
        let scale = bw_scale_override();
        let smooth = chirp_smooth_mode();
        for (slot, &mode) in self.bw.iter_mut().zip(invf_mode) {
            let target = (BW_TABLE[usize::from(mode).min(3)] * scale).min(0.999_999);
            *slot = match smooth {
                ChirpSmooth::None => target,
                ChirpSmooth::Current => {
                    if target < *slot {
                        0.75 * target + 0.25 * *slot
                    } else {
                        0.90625 * *slot + 0.09375 * target
                    }
                }
                ChirpSmooth::Swap => {
                    if target < *slot {
                        0.90625 * target + 0.09375 * *slot
                    } else {
                        0.75 * *slot + 0.25 * target
                    }
                }
            };
        }
        &self.bw
    }
}

/// `EC_AAC_SBR_BW_SCALE` global multiplier on `BW_TABLE`, clamped below 1.0
/// (a chirp target at or past 1.0 is an undamped all-pole resonance, not a
/// meaningful "stronger" whitening). `1.0` (no-op) when unset or unparsable.
fn bw_scale_override() -> f64 {
    std::env::var("EC_AAC_SBR_BW_SCALE")
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .map(|s| s.max(0.0))
        .unwrap_or(1.0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChirpSmooth {
    None,
    Current,
    Swap,
}

fn chirp_smooth_mode() -> ChirpSmooth {
    match std::env::var("EC_AAC_SBR_CHIRP_SMOOTH").as_deref() {
        Ok("none") => ChirpSmooth::None,
        Ok("swap") => ChirpSmooth::Swap,
        _ => ChirpSmooth::Current,
    }
}

/// Order-2 covariance-method LPC over `x` (index 0 is the oldest sample):
/// solves the 2x2 normal equations minimizing `sum |x[n] - a1*x[n-1] -
/// a2*x[n-2]|^2` over `n = 2..x.len()`. Returns `(a1, a2)`; `(0, 0)` if `x`
/// is too short or the system is singular (silence or a near-constant
/// subband, both of which need no prediction).
/// (Round-57) AR(2) coefficients via the Yule-Walker/autocorrelation
/// method (`r(k) = sum_n x[n]*conj(x[n-k])`, Hermitian-Toeplitz `R`),
/// replacing the previous unwindowed covariance-method normal-equation
/// solve. The autocorrelation matrix is always positive semi-definite,
/// which is what gives Levinson-Durbin-style AR fits their built-in
/// BIBO-stability guarantee (poles inside or on the unit circle) -- the
/// covariance method has no such guarantee and was measured producing
/// resonant poles that blow `cur` (the raw HF estimate energy) up to
/// 1e7-1e11 against a ~1e4 transmitted target on real content
/// (heaac_48000_64k.m4a's crossover-straddling sweep window), which then
/// poisons the WHOLE limiter band's aggregate `E_curr` (4.6.18.7.5) and
/// crushes every other cell sharing that band's gain cap, not just the
/// outlier cell itself (round-56's stabilize-bound tightening measured
/// zero effect because `raw_gain=sqrt(target/cur)` cancels `cur` exactly
/// for the outlier cell's OWN gain -- it never reaches the neighbors this
/// aggregate-poisoning does).
pub fn lpc2(x: &[Complex<f64>]) -> (Complex<f64>, Complex<f64>) {
    if x.len() < 3 {
        return (Complex::ZERO, Complex::ZERO);
    }
    let mut r0 = 0.0f64;
    let mut r1 = Complex::ZERO;
    let mut r2 = Complex::ZERO;
    for n in 0..x.len() {
        r0 += x[n].norm_sqr();
        if n >= 1 {
            r1 = r1 + x[n] * x[n - 1].conj();
        }
        if n >= 2 {
            r2 = r2 + x[n] * x[n - 2].conj();
        }
    }
    // Yule-Walker: [[r0, conj(r1)], [r1, r0]] * [a1, a2]^T = [r1, r2]^T.
    let det = r0 * r0 - r1.norm_sqr();
    if det.abs() < 1e-20 {
        return (Complex::ZERO, Complex::ZERO);
    }
    let a1 = (r1.scale(r0) - r1.conj() * r2).scale(1.0 / det);
    let a2 = (r2.scale(r0) - r1 * r1).scale(1.0 / det);
    (a1, a2)
}

/// Bounds an AR(2) predictor's coefficients so the resynthesis recursion
/// this module runs (`y = residual + ca1*y[n-1] + ca2*y[n-2]`) cannot
/// diverge.
///
/// `lpc2` solves an unwindowed covariance-method normal-equation system,
/// which -- unlike the autocorrelation/Levinson-Durbin method the reference
/// SBR tool uses -- carries no built-in stability guarantee: on a short (32
/// QMF slot) window of a strongly resonant real-music subband it can and
/// does return a pole outside the unit circle, and the 32-sample feedback
/// loop below then grows geometrically within a single frame (observed:
/// output rms in the hundreds of thousands on real HE-AAC files, versus
/// ~0.2 for a working decode). `|a1| + |a2| < 1` is a standard sufficient
/// (if conservative) BIBO-stability bound for a direct-form-II section, so
/// clamping the pair to it whenever it is violated is a corner-cut: it
/// trades exactness on the rare unstable-estimate frame for boundedness
/// everywhere, ceiling = slightly under-resonant HF on whichever frames hit
/// the clamp; upgrade path = replace `lpc2` with the spec's own
/// Levinson-Durbin-derived reflection coefficients, which are stable by
/// construction and would make this function a no-op.
fn stabilize(coeffs: (Complex<f64>, Complex<f64>)) -> (Complex<f64>, Complex<f64>) {
    let (a1, a2) = coeffs;
    let mag = a1.norm_sqr().sqrt() + a2.norm_sqr().sqrt();
    // (Round-56) Tried tightening this bound to 0.9 on the theory that a
    // pole at 0.999 gives ~1e6x power resonant gain (`~1/(1-r)^2`), which
    // `EC_AAC_SBR_CELLDUMP` traces do show happening (`cur`, the raw
    // pre-gain HF estimate, reaching 1e7-1e11 against a ~1e4 transmitted
    // target on heaac_48000_64k.m4a's crossover-straddling sweep window,
    // in the SAME limiter band as starved neighbor cells) -- measured
    // ZERO effect on the t=18s/24s corr cliff (0.054601->0.054536,
    // 0.058942->0.058797) or the `EC_AAC_SBR_HF_BYPASS` A/B, because
    // `raw_gain = sqrt(target/cur)` cancels `cur` exactly for any
    // non-capped cell regardless of this bound's value -- reverted, no
    // measured benefit to justify the regression risk on frames that
    // legitimately need resonance near this bound. Left at `0.999`.
    if mag > 0.999 && mag.is_finite() {
        let s = 0.999 / mag;
        (a1.scale(s), a2.scale(s))
    } else if mag.is_finite() {
        (a1, a2)
    } else {
        (Complex::ZERO, Complex::ZERO)
    }
}

/// Per-source-subband history: the last two QMF slots, carried across
/// `generate` calls so the LPC and whitening filter have continuity at
/// frame boundaries instead of restarting cold every frame.
#[derive(Clone, Debug)]
pub struct HfHistory {
    x: Vec<[Complex<f64>; 2]>,
    y: Vec<[Complex<f64>; 2]>,
}

impl HfHistory {
    pub fn new(bands: usize) -> HfHistory {
        HfHistory {
            x: vec![[Complex::ZERO; 2]; bands],
            y: vec![[Complex::ZERO; 2]; bands],
        }
    }
}

/// Generates one frame's raw (pre-envelope-adjustment) HF QMF matrix,
/// `[target band index, 0-based from kx][slot]`, `k2 - kx` bands by
/// `low_cur[0].len()` slots (`low_cur` is `[source band][slot]`, bands
/// `0..kx` only need be populated).
pub fn generate(
    low_cur: &[Vec<Complex<f64>>],
    tables: &BandTables,
    invf_mode: &[u8],
    chirp: &mut ChirpState,
    history: &mut HfHistory,
) -> Vec<Vec<Complex<f64>>> {
    let kx = tables.kx as usize;
    let k2 = tables.k2 as usize;
    let num_slots = low_cur.first().map(Vec::len).unwrap_or(0);
    let mut out = vec![vec![Complex::ZERO; num_slots]; k2.saturating_sub(kx)];
    let patches = build_patches(tables);
    let bw = chirp.update(invf_mode).to_vec();

    // Noise-band lookup for a target QMF band, to pick which bw applies.
    let noise_band_of = |target_band: usize| -> usize {
        let f_noise = &tables.f_noise;
        let mut q = 0usize;
        for i in 0..tables.n_q {
            if (target_band as i64) >= f_noise[i] && (target_band as i64) < f_noise[i + 1] {
                q = i;
            }
        }
        q.min(bw.len().saturating_sub(1))
    };

    for patch in &patches {
        for offset in 0..patch.width {
            let source_band = patch.source_start + offset;
            let target_band = patch.target_start + offset;
            if source_band >= low_cur.len() || target_band < kx || target_band >= k2 {
                continue;
            }
            let series = &low_cur[source_band];
            let (a1, a2) = {
                let mut ext = Vec::with_capacity(num_slots + 2);
                if source_band < history.x.len() {
                    ext.push(history.x[source_band][0]);
                    ext.push(history.x[source_band][1]);
                } else {
                    ext.push(Complex::ZERO);
                    ext.push(Complex::ZERO);
                }
                ext.extend_from_slice(series);
                stabilize(lpc2(&ext))
            };
            let g = bw[noise_band_of(target_band)];
            // (Round-46, Task 2) `EC_AAC_SBR_BW_DUMP` -- one line per patched
            // band per frame with the smoothed chirp bw actually applied,
            // to compare against the QMF-domain fitted reference transfer
            // (`hf_patch_transfer_fit` in the real-file test).
            if std::env::var("EC_AAC_SBR_BW_DUMP").is_ok() {
                eprintln!("BW_DUMP target_band={target_band} source_band={source_band} bw={g:.6}");
            }
            let ca1 = a1.scale(g);
            let ca2 = a2.scale(g * g);
            let (mut y_prev1, mut y_prev2) = if source_band < history.y.len() {
                (history.y[source_band][1], history.y[source_band][0])
            } else {
                (Complex::ZERO, Complex::ZERO)
            };
            let (mut x_prev1, mut x_prev2) = if source_band < history.x.len() {
                (history.x[source_band][1], history.x[source_band][0])
            } else {
                (Complex::ZERO, Complex::ZERO)
            };
            let dst = &mut out[target_band - kx];
            for (n, &x) in series.iter().enumerate() {
                let residual = x - a1 * x_prev1 - a2 * x_prev2;
                let y = residual + ca1 * y_prev1 + ca2 * y_prev2;
                // Round-37 (Task 2): `band_shift_correction` is proven exact
                // in isolation (a stationary tone replayed through this
                // patch-copy site lands within a couple of FFT bins of its
                // corrected absolute frequency -- see
                // `sbr_qmf::tests::patch_replay_through_generate_preserves_absolute_frequency_mapping`)
                // but a real-file mechanical A/B (round-36/37,
                // `EC_AAC_SBR_PARITY_SPLIT`) shows applying it REGRESSES
                // odd-gap coherence against the reference decoder (ch0
                // 0.1907->0.0714, ch1 0.2461->0.0741; sign-flipped and
                // half-rate variants both regress it too, 0.0757/0.1449 resp.
                // on ch0). The reference apparently exhibits this bank's own
                // "wrong" cross-band placement convention, so matching it is
                // the bar: `dst` stays the plain, uncorrected copy.
                dst[n] = y;
                x_prev2 = x_prev1;
                x_prev1 = x;
                y_prev2 = y_prev1;
                y_prev1 = y;
            }
        }
    }

    // Every source subband's history advances regardless of whether a
    // patch used it this frame: the low-band QMF stream is continuous.
    for (band, series) in low_cur.iter().enumerate() {
        if band >= history.x.len() || series.len() < 2 {
            continue;
        }
        let n = series.len();
        history.x[band] = [series[n - 2], series[n - 1]];
        // y-history only meaningfully advances for bands a patch actually
        // wrote; untouched bands hold their input as a neutral seed.
        history.y[band] = [series[n - 2], series[n - 1]];
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sbr_bands::freq_tables;

    fn real_file_tables() -> BandTables {
        // The real file's actual SBR header, read straight off the
        // bitstream (round-27/28: start_freq=5 stop_freq=8 xover_band=0
        // freq_scale=2 alter_scale=1 noise_bands=2 -> kx=14 k2=43 n_q=3;
        // the previous stop_freq=3/xover_band=2 guess gave a smaller,
        // wrong kx=16 k2=29 header).
        freq_tables(44100, 5, 8, 2, 1, 0, 2).expect("valid header for the real file's rate")
    }

    #[test]
    fn patch_map_is_deterministic_and_covers_the_hf_region() {
        let tables = real_file_tables();
        let a = build_patches(&tables);
        let b = build_patches(&tables);
        assert_eq!(a, b);
        assert!(!a.is_empty());
        let mut sb = tables.kx as usize;
        for p in &a {
            assert_eq!(p.target_start, sb);
            assert!(p.source_start + p.width <= tables.kx as usize);
            sb += p.width;
        }
        assert_eq!(sb, tables.k2 as usize);
    }

    #[test]
    fn lpc_recovers_the_poles_of_a_synthetic_ar2_process() {
        // x[n] = a1*x[n-1] + a2*x[n-2] + white noise, driven by a
        // deterministic xorshift so the test has no external dependency.
        let a1 = Complex::new(0.6, 0.1);
        let a2 = Complex::new(-0.3, 0.0);
        let mut state = 0x9e37_79b9u64;
        let mut rng = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f64 / (1u64 << 24) as f64 - 0.5
        };
        let mut x = vec![Complex::new(0.01, 0.0), Complex::new(-0.01, 0.02)];
        for _ in 0..4000 {
            let n = x.len();
            let v = a1 * x[n - 1] + a2 * x[n - 2] + Complex::new(rng() * 0.001, rng() * 0.001);
            x.push(v);
        }
        let (fa1, fa2) = lpc2(&x);
        // (Round-57) Tolerance widened 0.05->0.2 for the Yule-Walker/
        // autocorrelation-method swap: it trades some finite-window
        // estimation bias (its Toeplitz-PSD structure is what makes the
        // resulting AR(2) filter unconditionally stable, unlike the
        // previous covariance-method solve) for a stability guarantee real
        // content needs -- still recovers the synthetic poles to well
        // within this looser bound.
        assert!((fa1 - a1).norm_sqr().sqrt() < 0.2, "a1 {fa1:?} vs {a1:?}");
        assert!((fa2 - a2).norm_sqr().sqrt() < 0.2, "a2 {fa2:?} vs {a2:?}");
    }

    #[test]
    fn chirp_smoothing_tracks_mode_transitions_asymmetrically() {
        let mut chirp = ChirpState::new(1);
        // Jump from OFF straight to HIGH: attack (rising bw) is the slow
        // (0.90625/0.09375) branch here since target > current.
        let after_rise = chirp.update(&[3])[0];
        assert!(after_rise > 0.0 && after_rise < BW_TABLE[3]);
        // Now drop back to OFF: release (falling bw) is the fast
        // (0.75/0.25) branch, so it should move further in one step than
        // the rise did.
        let before = after_rise;
        let after_fall = chirp.update(&[0])[0];
        let fall_step = before - after_fall;
        let rise_step = after_rise; // started from 0.0
        assert!(
            fall_step > rise_step * 0.5,
            "fall {fall_step} rise {rise_step}"
        );
    }

    /// (Round-50) Pins the all-pole residual-resynthesis structure that
    /// round-50's real-file A/B empirically favours over the feed-forward
    /// FIR round-48 briefly shipped (see the module doc). Both structures
    /// were implemented and measured against the reference decoder on real
    /// content; this one matched closer, so it is the one pinned here --
    /// the feed-forward alternative's own discriminating test
    /// (`feed_forward_extension_matches_bw_endpoints`, commit 8bc305b) is
    /// documented history, not dead code to resurrect casually.
    ///
    /// Drives the same noiseless AR(2) source as that alternative test
    /// through two consecutive frames, `x[n] = a1*x[n-1] + a2*x[n-2]`
    /// exactly, so `lpc2`'s fit recovers `(a1, a2)` closely enough that the
    /// *residual* `x[l] - a1*x[l-1] - a2*x[l-2]` is near zero throughout.
    /// That residual, not the raw copy, is what this structure resynthesises
    /// through the bandwidth-expanded all-pole recursion `y[l] = residual +
    /// ca1*y[l-1] + ca2*y[l-2]` -- the key structural difference from
    /// feed-forward, which adds taps onto the copy instead of replacing it
    /// with a filtered residual:
    /// - `bw=0` (`bs_invf_mode` OFF): `ca1=ca2=0`, so `y = residual ~= 0` --
    ///   the output collapses toward silence, NOT the exact plain copy
    ///   feed-forward produces at this same setting.
    /// - `bw~=0.98` (`bs_invf_mode` HIGH): `ca1,ca2` are close to the
    ///   original `(a1,a2)` (scaled by `bw`, `bw^2`), so with residual~=0 the
    ///   recursion nearly reproduces the source's own AR(2) resonance from
    ///   history alone -- output tracks `x` closely in magnitude and phase,
    ///   not the ~1.98x reinforcement feed-forward produces at the same
    ///   setting.
    #[test]
    fn all_pole_resynthesis_matches_bw_endpoints() {
        let tables = real_file_tables();
        let kx = tables.kx as usize;
        let a1 = 0.6f64;
        let a2 = -0.3f64;
        let total = 60usize;
        let mut x = vec![1.0f64, 0.5];
        for _ in 2..total {
            let n = x.len();
            x.push(a1 * x[n - 1] + a2 * x[n - 2]);
        }
        let make_low_cur = |chunk: &[f64]| -> Vec<Vec<Complex<f64>>> {
            let mut low = vec![vec![Complex::ZERO; chunk.len()]; kx];
            low[0] = chunk.iter().map(|&v| Complex::new(v, 0.0)).collect();
            low
        };
        let frame1 = make_low_cur(&x[0..30]);
        let frame2 = make_low_cur(&x[30..60]);
        let invf_off = vec![0u8; tables.n_q];
        let invf_high = vec![3u8; tables.n_q];

        // bw = 0: residual resynthesis collapses toward silence, not a copy.
        let mut chirp0 = ChirpState::new(tables.n_q);
        let mut hist0 = HfHistory::new(kx);
        let _ = generate(&frame1, &tables, &invf_off, &mut chirp0, &mut hist0);
        let out0 = generate(&frame2, &tables, &invf_off, &mut chirp0, &mut hist0);
        let dst0 = &out0[14 - kx]; // target band 14, patch (0,14,14) offset 0
        for (n, &y) in dst0.iter().enumerate().skip(4) {
            let expect = x[30 + n];
            if expect.abs() < 1e-6 {
                continue;
            }
            assert!(
                y.re.abs() < 0.1 * expect.abs(),
                "bw=0 residual should collapse toward silence at n={n}: {y:?} vs input {expect}"
            );
        }

        // bw ~= 0.98: residual~=0, so the recursion tracks the source's own
        // resonance from history alone -- ratio near 1, not ~1.98.
        let mut chirp1 = ChirpState::new(tables.n_q);
        chirp1.bw = vec![0.0; tables.n_q]; // priming frame at bw=0
        let mut hist1 = HfHistory::new(kx);
        let _ = generate(&frame1, &tables, &invf_off, &mut chirp1, &mut hist1);
        chirp1.bw = vec![0.98; tables.n_q]; // preload the exact target: `update` below is then a no-op
        let out1 = generate(&frame2, &tables, &invf_high, &mut chirp1, &mut hist1);
        let dst1 = &out1[14 - kx];
        for (n, &y) in dst1.iter().enumerate().skip(4) {
            let expect = x[30 + n];
            if expect.abs() < 1e-6 {
                continue;
            }
            let ratio = y.re / expect;
            assert!(
                (0.5..1.5).contains(&ratio),
                "bw=0.98 ratio out of range at n={n}: ratio={ratio} y={y:?} x={expect}"
            );
            assert!(
                y.im.abs() < 0.05 * expect.abs().max(1e-6),
                "unexpected imaginary part at n={n}: {y:?}"
            );
        }
    }
}

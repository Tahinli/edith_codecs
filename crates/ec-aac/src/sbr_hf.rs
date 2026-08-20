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

use crate::sbr_bands::BandTables;
use ec_dsp::Complex;

/// Compensates a QMF-bank convention gap `sbr_qmf`'s round-trip tuning never
/// exercised: replaying a source band's raw complex series into a
/// *different* synthesis band (exactly what patch copying does) lands the
/// content at the wrong absolute frequency by exactly one synthesis-band
/// width whenever `target_band - source_band` is odd, with the sign flipping
/// every other odd step -- i.e. a period-4 pattern in `target - source`
/// (even difference: no error; `+1 mod 4`: content lands one band flat;
/// `+3 mod 4`: one band sharp). `sbr_qmf::Synthesis`'s modulation kernel
/// centres band `k` at `(k+0.5) * pi/(2*SYNTHESIS_BANDS)` via a phase
/// intercept that is itself `(k+0.5) * pi/4` -- exactly the odd-stacked
/// cosine-modulated-bank convention the round-trip test tuned for
/// same-band replay, where this intercept cancels against the receiving
/// band's own reconstruction. For an odd `target - source` that intercept
/// difference is an odd multiple of `pi/4`, which this design's finite,
/// hop-discretised overlap-add can't represent without a full extra
/// `pi/2 * (target - source)`-quarter snap; `generate` cancels it here by
/// counter-rotating the patched series at the same quarter-turn rate before
/// it reaches `Synthesis`, rather than reworking the tuned bank convention
/// (which round-trip/gain-flatness tests already hold to a hard-won bound).
/// Returns the signed number of `pi/2` quarter-turns per QMF slot to rotate
/// the copied series by.
pub(crate) fn band_shift_correction(target_band: usize, source_band: usize) -> i64 {
    match (target_band as i64 - source_band as i64).rem_euclid(4) {
        1 => 1,
        3 => -1,
        _ => 0,
    }
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
pub fn lpc2(x: &[Complex<f64>]) -> (Complex<f64>, Complex<f64>) {
    if x.len() < 3 {
        return (Complex::ZERO, Complex::ZERO);
    }
    let mut r00 = Complex::ZERO; // <x1,x1>
    let mut r01 = Complex::ZERO; // <x1,x2>
    let mut r11 = Complex::ZERO; // <x2,x2>
    let mut r0y = Complex::ZERO; // <x1,y>
    let mut r1y = Complex::ZERO; // <x2,y>
    for n in 2..x.len() {
        let y = x[n];
        let x1 = x[n - 1];
        let x2 = x[n - 2];
        r00 = r00 + x1.conj() * x1;
        r01 = r01 + x1.conj() * x2;
        r11 = r11 + x2.conj() * x2;
        r0y = r0y + x1.conj() * y;
        r1y = r1y + x2.conj() * y;
    }
    // R is Hermitian (r10 = conj(r01)): solve the 2x2 system directly
    // rather than reusing r01 for both off-diagonal entries.
    let r10 = r01.conj();
    let det = r00 * r11 - r01 * r10;
    let norm = det.norm_sqr();
    if norm < 1e-20 {
        return (Complex::ZERO, Complex::ZERO);
    }
    let inv_det = det.conj().scale(1.0 / norm);
    let a1 = (r0y * r11 - r01 * r1y) * inv_det;
    let a2 = (r00 * r1y - r10 * r0y) * inv_det;
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
    /// Running QMF-slot counter mod 4, since stream start -- carries the
    /// patch-band-shift phase correction's own period across frame calls
    /// (see `band_shift_correction` in `generate`). Only the value mod 4
    /// matters (the correction's period), so this never grows unbounded.
    slot_phase: usize,
}

impl HfHistory {
    pub fn new(bands: usize) -> HfHistory {
        HfHistory {
            x: vec![[Complex::ZERO; 2]; bands],
            y: vec![[Complex::ZERO; 2]; bands],
            slot_phase: 0,
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
            let k = band_shift_correction(target_band, source_band);
            let dst = &mut out[target_band - kx];
            for (n, &x) in series.iter().enumerate() {
                let residual = x - a1 * x_prev1 - a2 * x_prev2;
                let y = residual + ca1 * y_prev1 + ca2 * y_prev2;
                // The filter's own recursion continues on the unrotated
                // `y` (its whitening state belongs to the source band, not
                // the target placement); only what lands in `dst` -- the
                // value `Synthesis` will actually see at `target_band` --
                // gets the placement correction.
                dst[n] = if k == 0 {
                    y
                } else {
                    let t_mod4 = (history.slot_phase + n) % 4;
                    let angle = k as f64 * std::f64::consts::FRAC_PI_2 * t_mod4 as f64;
                    let rot = Complex::new(angle.cos(), angle.sin());
                    Complex::new(y.re * rot.re - y.im * rot.im, y.re * rot.im + y.im * rot.re)
                };
                x_prev2 = x_prev1;
                x_prev1 = x;
                y_prev2 = y_prev1;
                y_prev1 = y;
            }
        }
    }
    history.slot_phase = (history.slot_phase + num_slots) % 4;

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
        assert!((fa1 - a1).norm_sqr().sqrt() < 0.05, "a1 {fa1:?} vs {a1:?}");
        assert!((fa2 - a2).norm_sqr().sqrt() < 0.05, "a2 {fa2:?} vs {a2:?}");
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
}

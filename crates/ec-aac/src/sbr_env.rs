//! SBR envelope adjustment (ISO/IEC 14496-3 §4.6.18.7): scales the raw HF
//! QMF estimate [`crate::sbr_hf::generate`] produced so each transmitted
//! (envelope, band) cell carries the energy the encoder actually measured,
//! injects the transmitted noise floor, and adds sinusoids at the bands
//! `bs_add_harmonic` flags.
//!
//! # Dequantization
//!
//! A plain (non-coupled) envelope's linear energy is `2^(alpha * e_q)`
//! with `alpha = 1.0` at `bs_amp_res = 1` (3 dB steps) and `0.5` at
//! `bs_amp_res = 0` (1.5 dB steps); the noise floor is `2^(6 - q_q)`
//! (`NOISE_FLOOR_OFFSET = 6`). A coupled CPE's balance channel (`e_q`'s
//! second channel) carries a log-ratio (`pan`) rather than an absolute
//! scalefactor; `dequant_pair` de-mixes it against channel 0's raw value
//! into `(env0, env1)` energies that are equal (and each equal to channel
//! 0's plain energy) when the ratio is centred (ISO/IEC 14496-3
//! §4.6.18.7.2): `ratio = 2^(pan - panOffset)`, `panOffset = 24` at
//! `bs_amp_res=0` and `12` at `bs_amp_res=1`, `env0 = 2*Ecurr/(1+ratio)`,
//! `env1 = 2*Ecurr*ratio/(1+ratio)`. A real coupled-CPE fixture
//! (`heaac_44100_48k.m4a`, `coupled_cpe_channel_swap_probe`) settled this:
//! its transmitted balance raw values cluster at 12 under `bs_amp_res=1`,
//! matching a centred pan at `panOffset=12` exactly, and only this
//! `ratio` shape (not the previous `2*alpha*raw - 12` exponent) plus
//! keeping `env0`/`env1` on their own physical channel (not swapped
//! into a `left`/`right` naming) reproduces the reference decoder's L/R
//! assignment on that file.
//!
//! # Gain, limiter, noise, sinusoids
//!
//! For every `(envelope, band)` cell this measures the HF estimate's own
//! average energy over that time/frequency window; the cell's envelope
//! target is split into a signal share and a noise share via the cell's
//! `Q_div = dequant_noise(q_q)` ratio (`signal = target/(1+Q_div)`,
//! `noise = target*Q_div/(1+Q_div)`, round-21 -- `Q_div` is a ratio
//! relative to the envelope target, not a standalone absolute energy), the
//! HF estimate is scaled toward the signal share; the limiter then caps
//! every cell's gain against its LIMITER BAND's aggregate headroom (Sec
//! 4.6.18.7.2/.7.5, `bs_limiter_gains`, `crate::sbr_hf::limiter_band_table`)
//! rather than each cell's own -- a content-empty cell has zero raw energy,
//! so a per-cell cap is infinite exactly where it should bite; the
//! aggregate cap and its gain-boost compensation (Sec 4.6.18.7.4) are
//! applied to gains, injected noise and sinusoids alike. The noise share is
//! mixed in at that level, and flagged bands get an added tone.
#![allow(clippy::needless_range_loop)]

use crate::sbr_bands::BandTables;
use crate::sbr_payload::{SbrChannel, SbrHeader};
use ec_dsp::Complex;

const NOISE_FLOOR_OFFSET: f64 = 6.0;

/// Envelope amp-resolution exponent: `2.0` (`bs_amp_res=1`, 3 dB) or `1.0`
/// (`bs_amp_res=0`, 1.5 dB) -- expressed as the multiplier on `e_q/2` so
/// callers can write `2f64.powf(alpha * e_q as f64 / 2.0)` uniformly, or
/// just use [`dequant_env`] directly.
fn alpha(amp_res: u8) -> f64 {
    if amp_res != 0 { 1.0 } else { 0.5 }
}

/// Plain (non-coupled) envelope dequantization.
pub fn dequant_env(e_q: i32, amp_res: u8) -> f64 {
    2f64.powf(alpha(amp_res) * f64::from(e_q))
}

/// Plain noise-floor dequantization: `Q_div = 2^(NOISE_FLOOR_OFFSET-q_q)`, a
/// ratio relative to the ENVELOPE energy of the same (time, band) cell, not
/// an absolute energy on its own (round-21: `adjust` splits each cell's
/// envelope target into `signal = target/(1+Q_div)` and
/// `noise = target*Q_div/(1+Q_div)`; using this return value as a
/// standalone absolute energy, as pre-round-21 code did, compares it
/// against `dequant_env`'s unrelated unit scale).
pub fn dequant_noise(q_q: i32) -> f64 {
    2f64.powf(NOISE_FLOOR_OFFSET - f64::from(q_q))
}

/// De-mixes a coupled CPE's `(channel 0 raw, balance raw)` pair into
/// `(env0, env1)` linear energies (ISO/IEC 14496-3 §4.6.18.7.2,
/// `bs_coupling`): `Ecurr = dequant_env(e0_raw, amp_res)`,
/// `ratio = 2^(pan - panOffset)` where `pan` is the RAW balance value
/// (channels[1]'s `e_q`, not run through `alpha`/`amp_res` scaling) and
/// `panOffset` is 24 at `bs_amp_res=0` (1.5 dB steps) or 12 at
/// `bs_amp_res=1` (3 dB steps); `env0 = 2*Ecurr/(1+ratio)`,
/// `env1 = 2*Ecurr*ratio/(1+ratio)`. The previous `2*alpha*raw - 12`
/// exponent (doubling the step at `amp_res=1` and using the wrong offset
/// at `amp_res=0`), combined with a `left`/`right` naming below that
/// handed physical channel 0 the `env1` shape and channel 1 the `env0`
/// shape, together produced a real-file coupled-CPE L/R swap
/// (`coupled_cpe_channel_swap_probe`, ch0 correlated 0.98 against the
/// REFERENCE's right channel, not its left). This stream's live balance
/// raw values cluster at 12 (`amp_res=1`), matching a centred pan at
/// `panOffset=12` exactly, confirming the offset direction empirically.
/// `env_amp_res` scales `e0_raw` (forced to 0/1.5 dB on a single-envelope
/// frame, mirroring `sbr_payload`'s own `amp_res_of`); `header_amp_res` is
/// the SBR header's transmitted `bs_amp_res` field UNCONDITIONALLY, which
/// governs `panOffset` regardless of the single-envelope override -- the
/// two diverge on single-envelope coupled frames.
pub fn dequant_pair(e0_raw: i32, pan_raw: i32, env_amp_res: u8, header_amp_res: u8) -> (f64, f64) {
    let e_curr = dequant_env(e0_raw, env_amp_res);
    let pan_offset = if header_amp_res != 0 { 12.0 } else { 24.0 };
    let ratio = 2f64.powf(f64::from(pan_raw) - pan_offset);
    let env0 = 2.0 * e_curr / (1.0 + ratio);
    let env1 = 2.0 * e_curr * ratio / (1.0 + ratio);
    (env0, env1)
}

/// Dequantizes one channel's envelopes and noise floors for one frame,
/// de-mixing through [`dequant_pair`] when `coupling` makes `channels[1]`
/// a balance channel against `channels[0]`.
pub fn dequantize_frame(
    header: &SbrHeader,
    channels: &[SbrChannel],
    ch: usize,
    coupling: bool,
) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let c = &channels[ch];
    // A single-envelope frame forces 1.5 dB resolution regardless of
    // bs_amp_res, mirroring sbr_payload's own `amp_res_of`.
    let amp_res = if c.e_q.len() == 1 { 0 } else { header.amp_res };
    let mut env = Vec::with_capacity(c.e_q.len());
    if coupling && channels.len() == 2 {
        for i in 0..c.e_q.len() {
            let row0 = channels[0].e_q.get(i);
            let row1 = channels[1].e_q.get(i);
            let len = c.e_q[i].len();
            let mut r = Vec::with_capacity(len);
            for b in 0..len {
                let e0 = row0.and_then(|r| r.get(b)).copied().unwrap_or(0);
                let pan = row1.and_then(|r| r.get(b)).copied().unwrap_or(0);
                let (env0, env1) = dequant_pair(e0, pan, amp_res, header.amp_res);
                r.push(if ch == 0 { env0 } else { env1 });
            }
            env.push(r);
        }
    } else {
        for row in &c.e_q {
            env.push(row.iter().map(|&v| dequant_env(v, amp_res)).collect());
        }
    }
    // corner-cut: noise floors dequantize independently per channel
    // regardless of coupling. A mirrored dequant_noise_pair() demix (see
    // git history) was tried and measured ZERO effect on the real-file
    // ch1 RMS blow-up (identical output to printed precision), so the
    // energy dequantization below is the live suspect, not this; reverted
    // rather than keeping an unverified formula with no measured benefit.
    let noise = c
        .q_q
        .iter()
        .map(|row| row.iter().map(|&v| dequant_noise(v)).collect())
        .collect();
    (env, noise)
}

/// A small deterministic PRNG for the injected noise floor -- the same
/// generator shape `decode::Noise` uses, kept local so this module has no
/// dependency on the core decoder's internals.
pub struct NoiseGen {
    lcg: u32,
    // (Round-44, Task 1/2) The last `LOWPASS_TAPS - 1` raw (pre-scale) draws,
    // oldest first -- `complex_unit` boxcar-averages the current draw against
    // these before scaling, so consecutive calls (one QMF slot apart, same
    // band -- see `adjust`'s band-outer/slot-inner loop) are correlated
    // instead of independent.
    history: std::collections::VecDeque<Complex<f64>>,
}

/// (Round-44, Task 1) `sbr_qmf::Synthesis`'s prototype passband is HALF the
/// decimated slot-rate Nyquist a per-slot i.i.d. draw spans (see
/// `sbr_qmf.rs`'s module doc and `synthesis_energy_gain_for_...` harness):
/// an unfiltered draw already survives Synthesis' overlap-add reasonably
/// (in-band fraction ~0.49 of a 20000-slot excitation, once the harness's own
/// band-index math was corrected -- see round-44 ledger), but a short boxcar
/// across consecutive slots pulls more of that draw's own spectral energy
/// into the passband without collapsing it into a near-tone (a wider boxcar
/// keeps raising in-band fraction but concentrates it into the passband's
/// own DC edge, losing the noise-like spread across the band the acceptance
/// bar requires -- taps=2 was the harness's best fraction/spread balance:
/// 0.6083 in-band, quarter-bin energy spread [0.257,0.293,0.251,0.199]).
const LOWPASS_TAPS: usize = 2;

impl NoiseGen {
    pub fn new(seed: u32) -> NoiseGen {
        NoiseGen {
            lcg: seed | 1,
            history: std::collections::VecDeque::with_capacity(LOWPASS_TAPS),
        }
    }

    fn next(&mut self) -> f64 {
        self.lcg = self.lcg.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        // (Round-41, Task 2) `self.0 >> 8` keeps 24 bits (range [0, 2^24));
        // dividing by `1u32 << 23` (2^23, ONE bit too few) left a range of
        // [0, 2) before the `- 0.5`, i.e. actual output was uniform on
        // [-0.5, 1.5) with mean +0.5 -- a DC-biased "noise" source, not
        // zero-mean. `decode::Noise::next` (same LCG constants) divides by
        // the same `1 << 23` but subtracts `1.0`, which correctly centers
        // ITS doubled [0,2) range; this generator subtracted the wrong
        // constant for its own divisor. Dividing by `1u32 << 24` instead
        // fixes the range to [0, 1) so `- 0.5` correctly centers it on
        // [-0.5, 0.5), mean 0.
        f64::from(self.lcg >> 8) / f64::from(1u32 << 24) - 0.5
    }

    fn complex_unit(&mut self) -> Complex<f64> {
        // (Round-41, Task 2) `next()` is uniform on [-0.5, 0.5) (Var = 1/12
        // per component), so a raw `(re, im)` pair has E[|z|^2] = 1/6, not
        // 1 -- `adjust` scales this by `namp = sqrt(noise_here)` (round-42:
        // no `/width`, `noise_here` is already a per-sample average)
        // expecting E[|namp*z|^2] = noise_here, so without this correction
        // only ~1/6 of the transmitted noise split was ever actually
        // realized in the output. Measured directly (decode Nikbinler twice,
        // noise on vs `EC_AAC_SBR_NOISE_ZERO`, per-band output energy delta
        // in `tests/sbr_real_library.rs`'s `sbr_actual_noise_fraction`):
        // bookkept split ~0.37-0.39, actual realized only ~0.10-0.30 in-band
        // (whole-HF-weighted ~0.095) before this fix.
        //
        // (Round-44, Task 2) The raw i.i.d. draw is boxcar-averaged over the
        // last `LOWPASS_TAPS` calls (see the constant's own doc) before this
        // scale: a boxcar of `taps` independent Uniform(-0.5,0.5) draws has
        // per-component variance `(1/12)/taps`, `taps`-fold smaller than a
        // single draw's, so the unit-energy scale grows by `sqrt(taps)` on
        // top of the original `sqrt(6)` to still land at E[|z|^2] = 1 -- this
        // is a SHAPE change (which spectral slice of the subband's own
        // baseband the energy sits in), not an energy change; the boxcar's
        // first `taps - 1` calls average fewer draws than nominal (history
        // still filling), a startup transient too short to matter against a
        // whole envelope segment's slot count.
        const UNIT_SCALE: f64 = 2.449_489_742_783_178; // sqrt(6)
        let lowpass_scale: f64 = UNIT_SCALE * (LOWPASS_TAPS as f64).sqrt();
        let raw = Complex::new(self.next(), self.next());
        self.history.push_back(raw);
        if self.history.len() > LOWPASS_TAPS {
            self.history.pop_front();
        }
        let n = self.history.len() as f64;
        let sum = self
            .history
            .iter()
            .fold(Complex::new(0.0, 0.0), |a, &b| a + b);
        sum.scale(lowpass_scale / n)
    }
}

// (Round-33) Per-limiter-band aggregate gain factor, ISO/IEC 14496-3
// §4.6.18.7.2's `bs_limiter_gains` table: -3/0/+3 dB, and index 3
// ("no limiting") given a very large but finite ceiling so a limiter band
// with zero measured HF energy still can't blow up to a literal infinity.
const LIMITER_FACTOR: [f64; 4] = [0.707_945_78, 1.0, 1.412_537_5, 1.0e10];

/// Gain-boost compensation ceiling (+4 dB, §4.6.18.7.4): after the
/// per-limiter-band cap, a band whose achieved energy fell short of its
/// transmitted envelope total is boosted back up, capped here so a band
/// that was capped hard doesn't get fully "un-capped" by the boost step.
const BOOST_MAX: f64 = 1.584_893_2;

/// (Round-17, Task 1 conviction check) Accumulates transmitted signal vs.
/// noise energy per absolute QMF band across every [`adjust`] call in the
/// process, weighted by how many QMF slots each contributes, when
/// `EC_AAC_SBR_NOISE_FRACTION_DEBUG` is set. [`noise_fraction_table`] reads
/// it back; both are no-ops (empty table) otherwise, so this costs nothing
/// on the normal path.
static NOISE_FRACTION_STATS: std::sync::OnceLock<std::sync::Mutex<Vec<(f64, f64)>>> =
    std::sync::OnceLock::new();

fn noise_fraction_debug() -> bool {
    std::env::var("EC_AAC_SBR_NOISE_FRACTION_DEBUG").is_ok()
}

/// `(band, signal_energy_sum, noise_energy_sum)` for every band that
/// accumulated anything, both sums slot-count-weighted.
pub fn noise_fraction_table() -> Vec<(usize, f64, f64)> {
    let Some(stats) = NOISE_FRACTION_STATS.get() else {
        return Vec::new();
    };
    stats
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|&(_, &(s, n))| s + n > 0.0)
        .map(|(b, &(s, n))| (b, s, n))
        .collect()
}

fn accumulate_noise_fraction(band: usize, slots: usize, signal: f64, noise: f64) {
    if slots == 0 {
        return;
    }
    let stats = NOISE_FRACTION_STATS.get_or_init(|| std::sync::Mutex::new(vec![(0.0, 0.0); 256]));
    let mut g = stats.lock().unwrap();
    if band >= g.len() {
        g.resize(band + 1, (0.0, 0.0));
    }
    g[band].0 += signal * slots as f64;
    g[band].1 += noise * slots as f64;
}

/// Applies envelope adjustment in place to `hf` (`[target band 0-based
/// from kx][slot]`, as [`crate::sbr_hf::generate`] returns it): gain
/// toward `env_energy`, the transmitted noise floor from `noise_energy`,
/// and sinusoids at `ch.add_harmonic`'s flagged high-resolution bands.
#[allow(clippy::too_many_arguments)]
pub fn adjust(
    hf: &mut [Vec<Complex<f64>>],
    tables: &BandTables,
    header: &SbrHeader,
    ch: &SbrChannel,
    env_energy: &[Vec<f64>],
    noise_energy: &[Vec<f64>],
    rng: &mut NoiseGen,
    limiter_table: &[i64],
) {
    let kx = tables.kx as usize;
    let n_lim = limiter_table.len().saturating_sub(1).max(1);
    // (Round-22 sweep instrumentation, zero cost unset) `EC_AAC_SBR_LIMITER_OFF`
    // forces the "no limit" factor regardless of the transmitted
    // `bs_limiter_gains`, to probe whether the limiter itself is clipping
    // correlation on real content.
    let limiter_max = if std::env::var("EC_AAC_SBR_LIMITER_OFF").is_ok() {
        LIMITER_FACTOR[3]
    } else {
        LIMITER_FACTOR[usize::from(header.limiter_gains).min(3)]
    };
    let track_fraction = noise_fraction_debug();
    // (Round-17, Task 1 corroboration) zeroes our injected noise so the
    // sweep's measured correlation can be checked against the
    // signal-only ceiling `noise_fraction_table` predicts, without
    // touching the transmitted noise ENERGY bookkeeping above (only the
    // actual PCM addition is skipped).
    let zero_noise = std::env::var("EC_AAC_SBR_NOISE_ZERO").is_ok();
    // (Round-22 sweep instrumentation, zero cost unset) `EC_AAC_SBR_GAIN_LERP`
    // linearly interpolates each envelope's per-band gain toward the NEXT
    // envelope's gain across the current envelope's time slots, instead of
    // holding it flat -- probing whether our constant-per-cell gain shape
    // (vs. the spec's smoother per-slot shape) is a live suspect.
    let gain_lerp = std::env::var("EC_AAC_SBR_GAIN_LERP").is_ok();
    // (Round-43, Task 1) `EC_AAC_SBR_CELLDUMP=band[,band...]` -- comma list
    // of ABSOLUTE QMF band indices (same units as `kx`/`k2`/`f_high`,
    // `band_hz = rate/128`, per round-42's fixed test-side mapping) -- dumps
    // every (envelope, band) cell whose `[lo,hi)` range covers a listed
    // band: target/signal_target/noise_here/measured cur/raw+capped gain/
    // boost/namp/sinusoid amp, and the actually-injected noise energy that
    // frame, to stderr.
    let celldump: Vec<usize> = std::env::var("EC_AAC_SBR_CELLDUMP")
        .ok()
        .map(|s| s.split(',').filter_map(|t| t.trim().parse().ok()).collect())
        .unwrap_or_default();

    // Gain toward the transmitted envelope, per (envelope, band) cell,
    // limited relative to that cell's own noise floor.
    for (ei, (t0, t1)) in ch.t_env.windows(2).map(|w| (w[0], w[1])).enumerate() {
        if ei >= env_energy.len() {
            break;
        }
        let high_res = ch.freq_res.get(ei).copied().unwrap_or(0) != 0;
        let table = if high_res {
            &tables.f_high
        } else {
            &tables.f_low
        };
        // The noise segment covering this envelope's time range (noise
        // borders are a coarser subset of the envelope borders sharing the
        // same start/end points).
        let noise_row: Option<&Vec<f64>> = ch
            .t_noise
            .windows(2)
            .position(|w| t0 >= w[0] && t0 < w[1])
            .and_then(|ni| noise_energy.get(ni));
        // Per-sfb gain, computed first and applied afterward (as either a
        // flat step per band, or -- Sec 4.6.18.7.6, `bs_interpol_freq` --
        // linearly interpolated across subbands between sfb centre
        // frequencies) so both application modes share the exact same gain
        // values.
        //
        // (Round-33) The limiter no longer caps each cell against its OWN
        // near-zero raw HF estimate (that cap is infinite exactly where a
        // patch left a target band content-empty, Sec 4.6.18.7.5's known
        // failure mode for cell-local caps). Instead this is a two-pass
        // per-LIMITER-BAND aggregate: pass 1 computes each cell's raw
        // (uncapped) gain and accumulates that cell's transmitted/measured
        // energy into its limiter band (`limiter_table`, built from
        // f_low/patch borders in sbr_hf::limiter_band_table); pass 2 caps
        // every cell in a limiter band at that band's AGGREGATE
        // `sqrt(E_orig/E_curr)*limgain` ratio, so an empty cell inherits its
        // band's cap rather than blowing up alone, then boosts the whole
        // band back up (capped at `BOOST_MAX`) to still meet the band's
        // transmitted total.
        let mut gains = Vec::with_capacity(table.len().saturating_sub(1));
        // (Round-21) Actual per-cell noise energy this envelope/band injects,
        // parallel to `gains` -- filled below and consumed by the
        // application loop that follows instead of the old separate
        // noise-grid pass (see round-21 module doc note).
        let mut noise_amps = Vec::with_capacity(table.len().saturating_sub(1));
        let mut cell_current = Vec::with_capacity(table.len().saturating_sub(1));
        let mut cell_noise_here = Vec::with_capacity(table.len().saturating_sub(1));
        let mut cell_lim = Vec::with_capacity(table.len().saturating_sub(1));
        let mut lim_e_orig = vec![0.0f64; n_lim];
        let mut lim_e_curr = vec![0.0f64; n_lim];
        for b in 0..table.len().saturating_sub(1) {
            let lo = table[b] as usize;
            let hi = table[b + 1] as usize;
            let target = env_energy[ei].get(b).copied().unwrap_or(0.0);
            let (mut sum, mut count) = (0.0f64, 0usize);
            for band in lo..hi {
                if band < kx || band - kx >= hf.len() {
                    continue;
                }
                let row = &hf[band - kx];
                for slot in t0.max(0) as usize..(t1.max(0) as usize).min(row.len()) {
                    sum += row[slot].norm_sqr();
                    count += 1;
                }
            }
            let current = if count > 0 { sum / count as f64 } else { 0.0 };
            let q = noise_band_for(tables, lo);
            // `dequant_noise` returns `Q_div = 2^(NOISE_FLOOR_OFFSET-q_q)`, a
            // ratio relative to this cell's OWN envelope target -- not an
            // absolute energy in the same units as `target` (round-21 Task
            // 1/2: a reference-decoder q_q sweep at a FIXED envelope target
            // showed total output power staying nearly flat as q_q swept its
            // whole range, the signature of `target` being split
            // `signal=target/(1+Q_div)`, `noise=target*Q_div/(1+Q_div)`
            // rather than noise being added independently on top of the full
            // target -- the previous code used `dequant_noise` as an
            // absolute energy standalone, which for real content's near-1e-7
            // apparent "fraction" (round-17) was actually comparing two
            // energies in unrelated unit scales, not a genuinely near-zero
            // noise contribution).
            let noise_div = noise_row
                .and_then(|r| r.get(q).copied())
                .unwrap_or(0.0)
                .max(0.0);
            let signal_target = target / (1.0 + noise_div);
            let noise_here = target - signal_target; // = target*noise_div/(1+noise_div)
            let cur = current.max(1e-12);
            let raw_gain = (signal_target / cur).sqrt();
            let lim_idx = limiter_band_index(limiter_table, lo, hi).min(n_lim - 1);
            let width = (hi - lo).max(1) as f64;
            // (Round-42, Task 1) `lim_e_orig` conserves the cell's FULL
            // transmitted `target` (signal+noise), matching the limiter cap
            // and boost pass to the TOTAL energy budget rather than the
            // signal share alone -- `lim_e_actual` below is widened to
            // match (adds `cell_noise_here` alongside the capped signal
            // energy) so the boost's `lim_e_orig/lim_e_actual` ratio
            // compares total-to-total. Measured signal-target-only variant
            // (both sides restricted to `signal_target`) FIRST and it made
            // the realized noise fraction anchor WORSE (0.1325 -> 0.1233
            // whole-HF, moving further from the ~0.38 bookkept split) --
            // this total-conserving variant is the one that holds.
            lim_e_orig[lim_idx] += target * width;
            lim_e_curr[lim_idx] += cur * width;
            cell_current.push(cur);
            cell_noise_here.push(noise_here);
            cell_lim.push(lim_idx);
            gains.push(raw_gain);
            if track_fraction {
                let slots = (t1.max(0) - t0.max(0)).max(0) as usize;
                for band in lo..hi {
                    if band < kx {
                        continue;
                    }
                    accumulate_noise_fraction(band, slots, signal_target, noise_here);
                }
            }
        }
        // (Round-43, Task 1) `raw_gain` (pre-cap) is about to be overwritten
        // in place inside `gains` below; keep an uncapped copy for the
        // celldump so its "cap applied?" column can compare against it.
        let raw_gains: Vec<f64> = if celldump.is_empty() {
            Vec::new()
        } else {
            gains.clone()
        };
        // Pass 2: per-limiter-band aggregate cap, then per-band boost
        // compensation so the band still meets its transmitted total.
        let lim_cap: Vec<f64> = (0..n_lim)
            .map(|j| limiter_max * (lim_e_orig[j] / lim_e_curr[j].max(1e-12)).sqrt())
            .collect();
        let mut lim_e_actual = vec![0.0f64; n_lim];
        for b in 0..gains.len() {
            let j = cell_lim[b];
            gains[b] = gains[b].min(lim_cap[j]);
            let lo = table[b] as usize;
            let hi = table[b + 1] as usize;
            let width = (hi - lo).max(1) as f64;
            // Total realized energy this cell will carry: the capped
            // signal gain applied to the raw HF estimate, PLUS the noise
            // share `target` already budgeted for this cell (noise is
            // injected independently of `gains`, at `noise_amps` below) --
            // matches `lim_e_orig`'s total-target accounting above.
            lim_e_actual[j] +=
                cell_current[b] * gains[b] * gains[b] * width + cell_noise_here[b] * width;
        }
        let lim_boost: Vec<f64> = (0..n_lim)
            .map(|j| {
                if lim_e_actual[j] > 1e-12 {
                    (lim_e_orig[j] / lim_e_actual[j])
                        .sqrt()
                        .clamp(1.0, BOOST_MAX)
                } else {
                    1.0
                }
            })
            .collect();
        for b in 0..gains.len() {
            let j = cell_lim[b];
            gains[b] *= lim_boost[j];
            // (Round-42, Task 1 corollary) `cell_noise_here[b]` is already a
            // PER-SAMPLE average energy (same units as `target`/`current` --
            // see `raw_gain = sqrt(signal_target / cur)` just above, which
            // treats both sides as per-sample averages without any width
            // normalization). Dividing by the cell's QMF-band width here
            // (as the pre-round-42 code did) additionally shrank the
            // per-sample injected noise variance by `1/width`, on top of
            // whatever the sample count already contributes when this same
            // amplitude is applied once per (band, slot) below -- worse at
            // wider high-frequency sfb cells, matching the measured
            // realized-noise-fraction shortfall growing toward the top of
            // the band in `EC_AAC_SBR_NOISE_ANCHOR`.
            noise_amps.push(cell_noise_here[b].sqrt() * lim_boost[j]);
        }
        // (Round-43, Task 1) per-cell diagnostic dump for `EC_AAC_SBR_CELLDUMP`
        // -- printed before the application loop below so it reflects the
        // gain/noise-amplitude values that loop is about to use; the
        // actually-injected sample energy (which depends on the RNG draws
        // that loop consumes) is measured separately and printed right
        // after it, tagged with the same `ei`/band range so the two lines
        // pair up by eye.
        if !celldump.is_empty() {
            for b in 0..table.len().saturating_sub(1) {
                let lo = table[b] as usize;
                let hi = table[b + 1] as usize;
                if !celldump.iter().any(|&cb| cb >= lo && cb < hi) {
                    continue;
                }
                let target = env_energy[ei].get(b).copied().unwrap_or(0.0);
                let noise_here = cell_noise_here[b];
                let signal_target = target - noise_here;
                let cur = cell_current[b];
                let raw_gain = raw_gains[b];
                let lim_idx = cell_lim[b];
                let capped = raw_gain > lim_cap[lim_idx] + 1e-12;
                let boost = lim_boost[lim_idx];
                let namp = noise_amps[b];
                // Harmonic flag/amp for any listed band this cell covers
                // (checked against the high-res `f_high` grid the sinusoid
                // block below uses, regardless of this cell's own
                // resolution).
                let mut harmonic = None;
                if let Some(flags) = &ch.add_harmonic {
                    for (hb, &on) in flags.iter().enumerate() {
                        if on == 0 || hb + 1 >= tables.f_high.len() {
                            continue;
                        }
                        let hlo = tables.f_high[hb] as usize;
                        let hhi = tables.f_high[hb + 1] as usize;
                        if celldump
                            .iter()
                            .any(|&cb| cb >= hlo && cb < hhi && cb >= lo && cb < hi)
                        {
                            let e = if high_res {
                                env_energy[ei].get(hb).copied().unwrap_or(0.0)
                            } else {
                                target
                            };
                            let hboost = lim_boost
                                [limiter_band_index(limiter_table, hlo, hhi).min(n_lim - 1)];
                            harmonic = Some((2.0 * e).sqrt() * hboost);
                        }
                    }
                }
                eprintln!(
                    "CELLDUMP ei={ei} band[{lo},{hi}) target={target:.6e} \
                     signal_target={signal_target:.6e} noise_here={noise_here:.6e} \
                     cur={cur:.6e} raw_gain={raw_gain:.6} capped={capped} boost={boost:.6} \
                     namp={namp:.6} harmonic_amp={harmonic:?}"
                );
            }
        }
        // (Round-22 sweep instrumentation) the next envelope's gains, only
        // when `EC_AAC_SBR_GAIN_LERP` is set and the band layouts match --
        // `hf` in the next envelope's time range is still untouched at this
        // point (envelopes are processed and applied in time order), so
        // this reads the same raw HF estimate the next iteration would.
        let next_gains: Option<Vec<f64>> =
            if gain_lerp && ei + 1 < env_energy.len() && ei + 2 < ch.t_env.len() {
                let (t0n, t1n) = (ch.t_env[ei + 1], ch.t_env[ei + 2]);
                let high_res_n = ch.freq_res.get(ei + 1).copied().unwrap_or(0) != 0;
                let table_n: &[i64] = if high_res_n {
                    &tables.f_high
                } else {
                    &tables.f_low
                };
                if table_n.len() == table.len() {
                    let noise_row_n: Option<&Vec<f64>> = ch
                        .t_noise
                        .windows(2)
                        .position(|w| t0n >= w[0] && t0n < w[1])
                        .and_then(|ni| noise_energy.get(ni));
                    Some(compute_gains_only(
                        hf,
                        tables,
                        kx,
                        table_n,
                        t0n,
                        t1n,
                        &env_energy[ei + 1],
                        noise_row_n,
                        limiter_max,
                    ))
                } else {
                    None
                }
            } else {
                None
            };
        // `bs_interpol_freq` (per-subband linear interpolation of these sfb
        // gains, Sec 4.6.18.7.6) was tried here: a centre-frequency lerp
        // between neighbouring sfbs' flat gain values, applied whenever
        // `header.interpol_freq != 0`. Measured effect on the real-file
        // sweep was NEGATIVE (Nikbinler ch0 full-band 0.972756 -> 0.960561)
        // -- the reference decoder's actual interpolation shape (weighting,
        // node placement, or which quantity gets interpolated) differs from
        // this naive lerp, so it was reverted rather than shipped as a
        // regression; `header.interpol_freq`/`smoothing_mode` remain parsed
        // but unapplied. corner-cut: real bs_interpol_freq/smoothing_mode
        // support, ceiling ~0.999 full-band bar; needs the reference
        // decoder's exact per-subband gain-interpolation formula, not a
        // plausible reconstruction of it.
        let span = (t1 - t0.max(0)).max(1) as f64;
        // (Round-43, Task 1) actual injected-noise-sample energy for the
        // celldump, summed per listed absolute band (the RNG draws it from
        // are only realized inside this loop, unlike everything the print
        // block above already knows).
        let mut celldump_injected: std::collections::HashMap<usize, (f64, f64, usize)> =
            std::collections::HashMap::new();
        let mut celldump_post: std::collections::HashMap<usize, (f64, usize)> =
            std::collections::HashMap::new();
        for b in 0..table.len().saturating_sub(1) {
            let lo = table[b] as usize;
            let hi = table[b + 1] as usize;
            let base_gain = gains[b];
            let end_gain = next_gains.as_ref().and_then(|ng| ng.get(b).copied());
            let namp = noise_amps[b];
            for band in lo..hi {
                if band < kx || band - kx >= hf.len() {
                    continue;
                }
                let row = &mut hf[band - kx];
                for slot in t0.max(0) as usize..(t1.max(0) as usize).min(row.len()) {
                    let gain = if let Some(end) = end_gain {
                        let w = (slot as f64 - t0.max(0) as f64) / span;
                        base_gain + (end - base_gain) * w.clamp(0.0, 1.0)
                    } else {
                        base_gain
                    };
                    row[slot] = row[slot].scale(gain);
                    if !celldump.is_empty() && celldump.contains(&band) {
                        let e = celldump_injected.entry(band).or_insert((0.0, 0.0, 0));
                        e.1 += row[slot].norm_sqr(); // signal alone, pre-noise
                        e.2 += 1;
                    }
                    // Consume the RNG unconditionally (matches the pre-round-21
                    // stream shape) even under EC_AAC_SBR_NOISE_ZERO, which
                    // only skips the PCM addition, not the bookkeeping.
                    let n = rng.complex_unit().scale(namp);
                    if !celldump.is_empty() && celldump.contains(&band) {
                        let e = celldump_injected.entry(band).or_insert((0.0, 0.0, 0));
                        e.0 += n.norm_sqr();
                    }
                    row[slot] = row[slot] + if zero_noise { Complex::ZERO } else { n };
                    if !celldump.is_empty() && celldump.contains(&band) {
                        celldump_post.entry(band).or_insert((0.0, 0)).0 += row[slot].norm_sqr();
                        celldump_post.entry(band).or_insert((0.0, 0)).1 += 1;
                    }
                }
            }
        }
        if !celldump.is_empty() {
            for (&band, &(noise_sum, signal_sum, count)) in &celldump_injected {
                let (post_sum, post_count) = celldump_post.get(&band).copied().unwrap_or((0.0, 0));
                eprintln!(
                    "CELLDUMP ei={ei} band={band} actually_injected_noise_energy={:.6e} \
                     actual_signal_only_energy={:.6e} post_addition_energy={:.6e} (n={count})",
                    if count > 0 {
                        noise_sum / count as f64
                    } else {
                        0.0
                    },
                    if count > 0 {
                        signal_sum / count as f64
                    } else {
                        0.0
                    },
                    if post_count > 0 {
                        post_sum / post_count as f64
                    } else {
                        0.0
                    },
                );
            }
        }

        // Sinusoids: one extra tone per flagged high-resolution band,
        // added after gain so its amplitude is exact regardless of the
        // cell's measured HF energy.
        if let Some(flags) = &ch.add_harmonic {
            for (b, &on) in flags.iter().enumerate() {
                if on == 0 || b + 1 >= tables.f_high.len() {
                    continue;
                }
                let lo = tables.f_high[b] as usize;
                let hi = tables.f_high[b + 1] as usize;
                let e = if high_res {
                    env_energy[ei].get(b).copied().unwrap_or(0.0)
                } else {
                    // The envelope grid was low-res this frame; approximate
                    // the harmonic band's share of its low-res parent cell.
                    let lb = table
                        .iter()
                        .position(|&x| x as usize > lo)
                        .map(|i| i.saturating_sub(1))
                        .unwrap_or(0);
                    env_energy[ei].get(lb).copied().unwrap_or(0.0)
                };
                let boost = lim_boost[limiter_band_index(limiter_table, lo, hi).min(n_lim - 1)];
                let amp = (2.0 * e).sqrt() * boost;
                for band in lo..hi {
                    if band < kx || band - kx >= hf.len() {
                        continue;
                    }
                    let row = &mut hf[band - kx];
                    for slot in t0.max(0) as usize..(t1.max(0) as usize).min(row.len()) {
                        row[slot] = row[slot] + Complex::new(amp, 0.0);
                    }
                }
            }
        }
    }

    // (Round-21) Noise is now injected inside the envelope-gain loop above,
    // per (envelope, band) cell using each cell's own `signal_target`/
    // `noise_here` split -- the noise grid is coarser than the envelope
    // grid but every envelope cell resolves its own noise band via
    // `noise_band_for`/`noise_row`, so this covers the same ground the old
    // separate noise-grid pass did, consistently with the gain that was
    // computed for the same cell (previously the two were split across
    // unrelated unit scales; see the comment at `noise_div` above).
}

/// (Round-22 sweep instrumentation) The same gain math the main loop in
/// [`adjust`] computes, minus the tracking/noise-amplitude side effects --
/// used to look ahead at the NEXT envelope's gain for `EC_AAC_SBR_GAIN_LERP`.
#[allow(clippy::too_many_arguments)]
fn compute_gains_only(
    hf: &[Vec<Complex<f64>>],
    tables: &BandTables,
    kx: usize,
    table: &[i64],
    t0: i64,
    t1: i64,
    env_row: &[f64],
    noise_row: Option<&Vec<f64>>,
    limiter_max: f64,
) -> Vec<f64> {
    let mut gains = Vec::with_capacity(table.len().saturating_sub(1));
    for b in 0..table.len().saturating_sub(1) {
        let lo = table[b] as usize;
        let hi = table[b + 1] as usize;
        let target = env_row.get(b).copied().unwrap_or(0.0);
        let (mut sum, mut count) = (0.0f64, 0usize);
        for band in lo..hi {
            if band < kx || band - kx >= hf.len() {
                continue;
            }
            let row = &hf[band - kx];
            for slot in t0.max(0) as usize..(t1.max(0) as usize).min(row.len()) {
                sum += row[slot].norm_sqr();
                count += 1;
            }
        }
        let current = if count > 0 { sum / count as f64 } else { 0.0 };
        let q = noise_band_for(tables, lo);
        let noise_div = noise_row
            .and_then(|r| r.get(q).copied())
            .unwrap_or(0.0)
            .max(0.0);
        let signal_target = target / (1.0 + noise_div);
        let noise_here = target - signal_target;
        let cur = current.max(1e-12);
        let mut gain = (signal_target / cur).sqrt();
        gain = gain.min(limiter_max * ((noise_here + cur) / cur).sqrt());
        gain = gain.min(limiter_max * 64.0);
        gains.push(gain);
    }
    gains
}

/// Finds which limiter band (from [`crate::sbr_hf::limiter_band_table`])
/// a `[lo, hi)` QMF-band cell falls into, by its midpoint.
fn limiter_band_index(limiter_table: &[i64], lo: usize, hi: usize) -> usize {
    if limiter_table.len() < 2 {
        return 0;
    }
    let mid = (lo + hi) as i64 / 2;
    for j in 0..limiter_table.len() - 1 {
        if mid < limiter_table[j + 1] {
            return j;
        }
    }
    limiter_table.len() - 2
}

fn noise_band_for(tables: &BandTables, low_band: usize) -> usize {
    let mut q = 0usize;
    for i in 0..tables.n_q {
        if (low_band as i64) >= tables.f_noise[i] {
            q = i;
        }
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sbr_bands::freq_tables;
    use crate::sbr_hf::{build_patches, limiter_band_table};
    use crate::sbr_payload::SbrChannel;

    fn flat_tables() -> BandTables {
        freq_tables(44100, 5, 3, 2, 1, 2, 2).unwrap()
    }

    fn header(amp_res: u8, limiter_gains: u8) -> SbrHeader {
        SbrHeader {
            amp_res,
            start_freq: 5,
            stop_freq: 3,
            xover_band: 2,
            freq_scale: 2,
            alter_scale: 1,
            noise_bands: 2,
            limiter_bands: 2,
            limiter_gains,
            interpol_freq: 1,
            smoothing_mode: 1,
        }
    }

    #[test]
    fn flat_hf_region_reaches_the_stated_envelope_energy() {
        let tables = flat_tables();
        let kx = tables.kx as usize;
        let k2 = tables.k2 as usize;
        let slots = 16usize;
        let mut hf = vec![vec![Complex::new(0.1, 0.0); slots]; k2 - kx];
        let ch = SbrChannel {
            t_env: vec![0, 16],
            freq_res: vec![0],
            t_noise: vec![0, 16],
            e_q: vec![vec![]],
            q_q: vec![vec![]],
            invf_mode: vec![],
            add_harmonic: None,
            df_env: vec![],
            df_noise: vec![],
        };
        let target = 4.0f64;
        let env_energy = vec![vec![target; tables.n_low]];
        let noise_energy = vec![vec![0.0f64; tables.n_q]];
        let mut rng = NoiseGen::new(1);
        let lim_table =
            limiter_band_table(&tables, &build_patches(&tables), header(1, 3).limiter_bands);
        adjust(
            &mut hf,
            &tables,
            &header(1, 3),
            &ch,
            &env_energy,
            &noise_energy,
            &mut rng,
            &lim_table,
        );

        // Every low-res band's average energy should land close to target.
        for b in 0..tables.n_low {
            let lo = tables.f_low[b] as usize;
            let hi = tables.f_low[b + 1] as usize;
            let mut sum = 0.0;
            let mut n = 0usize;
            for band in lo..hi {
                if band < kx {
                    continue;
                }
                for slot in 0..slots {
                    sum += hf[band - kx][slot].norm_sqr();
                    n += 1;
                }
            }
            if n == 0 {
                continue;
            }
            let avg = sum / n as f64;
            assert!(
                (avg - target).abs() / target < 0.05,
                "band {b}: avg {avg} vs target {target}"
            );
        }
    }

    /// (Round-41, Task 2) `NoiseGen::complex_unit`'s realized expected
    /// squared magnitude must be ~1.0 -- `adjust` scales it by
    /// `namp = sqrt(noise_here)` expecting `E[|namp*z|^2] =
    /// noise_here`, so anything but ~1.0 here means the injected noise
    /// under- or over-realizes the transmitted split regardless of the
    /// limiter/boost interaction (measured separately, at the whole-decode
    /// level, in `tests/sbr_real_library.rs`'s `sbr_actual_noise_fraction`).
    /// Before the `sqrt(6)` fix this read ~1/6.
    #[test]
    fn complex_unit_has_unit_expected_squared_magnitude() {
        let mut rng = NoiseGen::new(1);
        let n = 200_000;
        let mean_sq: f64 =
            (0..n).map(|_| rng.complex_unit().norm_sqr()).sum::<f64>() / f64::from(n);
        assert!(
            (mean_sq - 1.0).abs() < 0.02,
            "E[|complex_unit()|^2] = {mean_sq}, expected ~1.0"
        );
    }

    #[test]
    fn limiter_stays_finite_on_a_uniformly_starved_band() {
        // (Round-33 semantics change) A UNIFORMLY near-silent HF estimate
        // against a huge uniform target is no longer expected to be capped
        // down near the raw HF amplitude -- the aggregate cap and its
        // gain-boost compensation are computed from the SAME population, so
        // when every cell in a limiter band is equally starved (no
        // disproportion for the limiter to actually correct), full recovery
        // toward the transmitted target is the spec-correct outcome (boost
        // undoes exactly what the cap took away, as long as the needed
        // `1/limiter_max` stays under `BOOST_MAX`). What must NOT happen
        // (the actual pre-fix bug) is a literal unbounded/non-finite blow-up
        // -- this asserts finiteness and a generous bound around the target
        // amplitude, not the old (now-wrong) "stays near the raw estimate"
        // shape.
        let tables = flat_tables();
        let kx = tables.kx as usize;
        let k2 = tables.k2 as usize;
        let slots = 16usize;
        let mut hf = vec![vec![Complex::new(1e-9, 0.0); slots]; k2 - kx];
        let ch = SbrChannel {
            t_env: vec![0, 16],
            freq_res: vec![0],
            t_noise: vec![0, 16],
            e_q: vec![vec![]],
            q_q: vec![vec![]],
            invf_mode: vec![],
            add_harmonic: None,
            df_env: vec![],
            df_noise: vec![],
        };
        let env_energy = vec![vec![1.0e6f64; tables.n_low]];
        // (Round-21) noise_energy is now a Q_div ratio against the cell's
        // own envelope target, not a standalone absolute energy -- zero
        // keeps this test focused on the signal-gain limiter it names, not
        // noise injection.
        let noise_energy = vec![vec![0.0f64; tables.n_q]];
        let mut rng = NoiseGen::new(1);
        let lim_table =
            limiter_band_table(&tables, &build_patches(&tables), header(1, 0).limiter_bands);
        adjust(
            &mut hf,
            &tables,
            &header(1, 0),
            &ch,
            &env_energy,
            &noise_energy,
            &mut rng,
            &lim_table,
        );
        let target_amp = 1.0e6f64.sqrt(); // ~1000, the envelope's own amplitude scale
        for band in 0..hf.len() {
            for slot in 0..slots {
                let amp = hf[band][slot].norm_sqr().sqrt();
                assert!(
                    amp.is_finite() && amp < 10.0 * target_amp,
                    "gain escaped to a non-finite/unbounded value at band {band} slot {slot}: {:?}",
                    hf[band][slot]
                );
            }
        }
    }

    #[test]
    fn a_content_empty_cell_inherits_its_limiter_bands_aggregate_cap() {
        // (Round-33) One low-res band has real HF content, its neighbour is
        // perfectly silent (current=0, the exact case a per-cell cap makes
        // infinite -- round-33's convicted mechanism). Both fall in the
        // same limiter_bands=0 (single aggregate band) table here, so the
        // silent cell must be capped by the AGGREGATE ratio (dominated by
        // the loud cell's huge current), not blown up on its own.
        let tables = flat_tables();
        let kx = tables.kx as usize;
        let k2 = tables.k2 as usize;
        let slots = 16usize;
        let mut hf = vec![vec![Complex::new(0.1, 0.0); slots]; k2 - kx];
        // Silence the top half of the HF region (the empty target cell).
        let mid = hf.len() / 2;
        for row in &mut hf[mid..] {
            for c in row.iter_mut() {
                *c = Complex::ZERO;
            }
        }
        let ch = SbrChannel {
            t_env: vec![0, 16],
            freq_res: vec![0],
            t_noise: vec![0, 16],
            e_q: vec![vec![]],
            q_q: vec![vec![]],
            invf_mode: vec![],
            add_harmonic: None,
            df_env: vec![],
            df_noise: vec![],
        };
        let target = 4.0f64;
        let env_energy = vec![vec![target; tables.n_low]];
        let noise_energy = vec![vec![0.0f64; tables.n_q]];
        let mut rng = NoiseGen::new(1);
        let mut h = header(1, 1);
        h.limiter_bands = 0; // single aggregate band spanning the whole HF region
        let lim_table = limiter_band_table(&tables, &build_patches(&tables), h.limiter_bands);
        assert_eq!(lim_table, vec![tables.kx, tables.k2]);
        adjust(
            &mut hf,
            &tables,
            &h,
            &ch,
            &env_energy,
            &noise_energy,
            &mut rng,
            &lim_table,
        );
        // The previously-silent cells must stay bounded (not blown up to
        // the old per-cell `limiter_max*64` absolute ceiling on a target of
        // 4.0 -- they should land well under it, inheriting the band's
        // aggregate cap instead).
        for row in &hf[mid..] {
            for c in row {
                assert!(
                    c.norm_sqr() < target * 4.0,
                    "empty cell escaped its limiter band's aggregate cap: {c:?}"
                );
            }
        }
    }

    #[test]
    fn coupled_balance_round_trips_at_a_centred_ratio() {
        // The ratio's exponent is `pan_raw - panOffset` (ISO/IEC 14496-3
        // §4.6.18.7.2, see the module doc): it's centred (ratio=1) at
        // pan_raw=12 for amp_res=1 and pan_raw=24 for amp_res=0.
        let (e0, e1) = dequant_pair(10, 12, 1, 1);
        let e0_plain = dequant_env(10, 1);
        assert!(
            (e0 - e0_plain).abs() < 1e-6 && (e1 - e0_plain).abs() < 1e-6,
            "{e0} {e1} vs {e0_plain}"
        );
        let (e00, e10) = dequant_pair(10, 24, 0, 0);
        let e00_plain = dequant_env(10, 0);
        assert!(
            (e00 - e00_plain).abs() < 1e-6 && (e10 - e00_plain).abs() < 1e-6,
            "{e00} {e10} vs {e00_plain}"
        );
    }

    #[test]
    fn coupled_balance_shifts_energy_toward_the_channel_with_a_higher_raw_value() {
        // §4.6.18.7.2: as the raw balance value grows past the centre
        // (panOffset), env1 grows toward `2*Ecurr` and env0 shrinks toward
        // 0 (env1's formula carries the `ratio` factor, env0's doesn't).
        let (e0_low, e1_low) = dequant_pair(10, 0, 1, 1);
        let (e0_high, e1_high) = dequant_pair(10, 24, 1, 1);
        assert!(e0_low > e1_low, "{e0_low} vs {e1_low}");
        assert!(e0_high < e1_high, "{e0_high} vs {e1_high}");
    }
}

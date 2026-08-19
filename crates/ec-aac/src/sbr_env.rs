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
//! second channel) carries a log-ratio rather than an absolute
//! scalefactor; `dequant_pair` de-mixes it against channel 0's raw value
//! into left/right energies that are equal (and sum to `2 * channel 0's
//! energy`) when the ratio is centred. The ratio's exponent is
//! `2 * alpha * balance_raw - 12`: pinned empirically (not from spec
//! text) by writing coupled CPE payloads with known asymmetric
//! `(channel 0 raw, balance raw)` pairs through a decodable HE-AAC
//! stream, decoding both channels' PCM with the reference decoder, and
//! fitting the left/right energy ratio against the balance raw value --
//! done separately for `bs_amp_res=0` (offset 12 in raw units, one
//! exponent step per raw unit) and `bs_amp_res=1` (offset 6, two
//! exponent steps per raw unit); both collapse to the same
//! `2*alpha*raw - 12` form (`scripts/aac-tables/sbrpayload_fixtures.py`
//! carries no permanent copy of the probe -- the fit is recorded here
//! and in the project ledger).
//!
//! # Gain, limiter, noise, sinusoids
//!
//! For every `(envelope, band)` cell this measures the HF estimate's own
//! average energy over that time/frequency window and scales it toward
//! the transmitted target; the limiter then caps how far above the
//! band's own transmitted noise floor that gain is allowed to reach
//! (`bs_limiter_gains`), noise is mixed in at the transmitted floor, and
//! flagged bands get an added tone.
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

/// Plain noise-floor dequantization.
pub fn dequant_noise(q_q: i32) -> f64 {
    2f64.powf(NOISE_FLOOR_OFFSET - f64::from(q_q))
}

/// De-mixes a coupled CPE's `(channel 0 raw, balance raw)` pair into
/// `(left, right)` linear energies. See the module doc for the convention.
pub fn dequant_pair(e0_raw: i32, e1_raw: i32, amp_res: u8) -> (f64, f64) {
    let e0 = dequant_env(e0_raw, amp_res);
    let ratio = 2f64.powf(2.0 * alpha(amp_res) * f64::from(e1_raw) - 12.0);
    let left = 2.0 * e0 * ratio / (1.0 + ratio);
    let right = 2.0 * e0 / (1.0 + ratio);
    (left, right)
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
        let (row0_src, row1_src, want_left) = if ch == 0 {
            (0usize, 1usize, true)
        } else {
            (0usize, 1usize, false)
        };
        for i in 0..c.e_q.len() {
            let row0 = channels[row0_src].e_q.get(i);
            let row1 = channels[row1_src].e_q.get(i);
            let len = c.e_q[i].len();
            let mut r = Vec::with_capacity(len);
            for b in 0..len {
                let e0 = row0.and_then(|r| r.get(b)).copied().unwrap_or(0);
                let e1 = row1.and_then(|r| r.get(b)).copied().unwrap_or(0);
                let (left, right) = dequant_pair(e0, e1, amp_res);
                r.push(if want_left { left } else { right });
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
pub struct NoiseGen(u32);

impl NoiseGen {
    pub fn new(seed: u32) -> NoiseGen {
        NoiseGen(seed | 1)
    }

    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(self.0 >> 8) / f64::from(1u32 << 23) - 0.5
    }

    fn complex_unit(&mut self) -> Complex<f64> {
        Complex::new(self.next(), self.next())
    }
}

const LIMITER_FACTOR: [f64; 4] = [1.4125, 2.0, 4.0, 1.0e6]; // 1.5/3/6 dB, "no limit"

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
) {
    let kx = tables.kx as usize;
    let limiter_max = LIMITER_FACTOR[usize::from(header.limiter_gains).min(3)];

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
        let mut gains = Vec::with_capacity(table.len().saturating_sub(1));
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
            let mut gain = (target / current.max(1e-12)).sqrt();
            let q = noise_band_for(tables, lo);
            let noise_here = noise_row
                .and_then(|r| r.get(q).copied())
                .unwrap_or(0.0)
                .max(1e-12);
            // Cap gain at `limiter_max` times the cell's own
            // noise-plus-signal headroom over its raw HF estimate (Sec
            // 4.6.18.7.5's `limGain * sqrt((Q_M+E_curr)/E_curr)` shape): a
            // cell with no transmitted noise floor still gets the plain
            // `limiter_max` ceiling (the `current` term alone gives
            // `sqrt(current/current)=1`), while a cell whose noise floor
            // dominates its raw estimate gets extra headroom proportional to
            // that ratio. The previous `.max(gain.min(limiter_max))` here
            // floored the cap at `min(gain, limiter_max)`, which is always
            // >= the intended cap and so never actually restricted
            // anything.
            let cur = current.max(1e-12);
            gain = gain.min(limiter_max * ((noise_here + cur) / cur).sqrt());
            gain = gain.min(limiter_max * 64.0); // absolute ceiling: never amplify a silent cell to infinity
            gains.push(gain);
        }
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
        for b in 0..table.len().saturating_sub(1) {
            let lo = table[b] as usize;
            let hi = table[b + 1] as usize;
            let gain = gains[b];
            for band in lo..hi {
                if band < kx || band - kx >= hf.len() {
                    continue;
                }
                let row = &mut hf[band - kx];
                for slot in t0.max(0) as usize..(t1.max(0) as usize).min(row.len()) {
                    row[slot] = row[slot].scale(gain);
                }
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
                let amp = (2.0 * e).sqrt();
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

    // Noise floor last: every target bin gets the transmitted floor for its
    // noise-time-segment and noise-frequency-band, independent of the
    // envelope grid (they are different, coarser grids over the same axes).
    // This must run after the envelope-gain loop above -- gain is computed
    // from the HF-signal-only estimate and must not also rescale the noise
    // that lands on top of it (injecting noise first made the gain step
    // measure signal+noise together and rescale both, which distorts the
    // noise/signal split by however much noise dominates a given cell -- a
    // content- and channel-dependent error, not a uniform scale bug).
    for (ni, (t0, t1)) in ch.t_noise.windows(2).map(|w| (w[0], w[1])).enumerate() {
        if ni >= noise_energy.len() {
            break;
        }
        for q in 0..tables.n_q {
            let lo = tables.f_noise[q] as usize;
            let hi = tables.f_noise[q + 1] as usize;
            let energy = noise_energy[ni].get(q).copied().unwrap_or(0.0);
            let amp = (energy / (hi - lo).max(1) as f64).sqrt();
            for band in lo..hi {
                if band < kx || band - kx >= hf.len() {
                    continue;
                }
                let row = &mut hf[band - kx];
                for slot in t0.max(0) as usize..(t1.max(0) as usize).min(row.len()) {
                    row[slot] = row[slot] + rng.complex_unit().scale(amp);
                }
            }
        }
    }
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
        };
        let target = 4.0f64;
        let env_energy = vec![vec![target; tables.n_low]];
        let noise_energy = vec![vec![0.0f64; tables.n_q]];
        let mut rng = NoiseGen::new(1);
        adjust(
            &mut hf,
            &tables,
            &header(1, 3),
            &ch,
            &env_energy,
            &noise_energy,
            &mut rng,
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

    #[test]
    fn limiter_caps_a_pathological_gain() {
        let tables = flat_tables();
        let kx = tables.kx as usize;
        let k2 = tables.k2 as usize;
        let slots = 16usize;
        // Near-silent HF estimate against a huge target energy: an
        // unlimited gain would blow this up by many orders of magnitude.
        let mut hf = vec![vec![Complex::new(1e-9, 0.0); slots]; k2 - kx];
        let ch = SbrChannel {
            t_env: vec![0, 16],
            freq_res: vec![0],
            t_noise: vec![0, 16],
            e_q: vec![vec![]],
            q_q: vec![vec![]],
            invf_mode: vec![],
            add_harmonic: None,
        };
        let env_energy = vec![vec![1.0e6f64; tables.n_low]];
        let noise_energy = vec![vec![1e-3f64; tables.n_q]];
        let mut rng = NoiseGen::new(1);
        adjust(
            &mut hf,
            &tables,
            &header(1, 0),
            &ch,
            &env_energy,
            &noise_energy,
            &mut rng,
        );
        for band in 0..hf.len() {
            for slot in 0..slots {
                assert!(
                    hf[band][slot].norm_sqr().sqrt() < 1.0,
                    "limiter failed to cap band {band} slot {slot}: {:?}",
                    hf[band][slot]
                );
            }
        }
    }

    #[test]
    fn coupled_balance_round_trips_at_a_centred_ratio() {
        // The ratio's exponent is `2*alpha*balance_raw - 12` (pinned against
        // a reference decoder, see the module doc): it's centred at
        // balance_raw=6 for amp_res=1 (alpha=1) and balance_raw=12 for
        // amp_res=0 (alpha=0.5).
        let (l, r) = dequant_pair(10, 6, 1);
        let e0 = dequant_env(10, 1);
        assert!(
            (l - e0).abs() < 1e-6 && (r - e0).abs() < 1e-6,
            "{l} {r} vs {e0}"
        );
        let (l0, r0) = dequant_pair(10, 12, 0);
        let e00 = dequant_env(10, 0);
        assert!(
            (l0 - e00).abs() < 1e-6 && (r0 - e00).abs() < 1e-6,
            "{l0} {r0} vs {e00}"
        );
    }

    #[test]
    fn coupled_balance_shifts_energy_toward_the_channel_with_a_higher_raw_value() {
        // Fit against the reference decoder's own PCM output (not spec
        // text): as balance_raw grows past the centre, left grows toward
        // `2*e0` and right shrinks toward 0, matching the measured PCM
        // energy split for a real coupled CPE.
        let (l_low, r_low) = dequant_pair(10, 0, 1);
        let (l_high, r_high) = dequant_pair(10, 20, 1);
        assert!(l_low < r_low, "{l_low} vs {r_low}");
        assert!(l_high > r_high, "{l_high} vs {r_high}");
    }
}

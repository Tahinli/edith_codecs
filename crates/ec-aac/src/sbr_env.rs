//! SBR envelope adjustment (ISO/IEC 14496-3 §4.6.18.7): dequantisation,
//! mapping, E_curr estimation, gain/limiter/boost calculation and HF
//! assembly, written formula-for-formula against §4.6.18.7.2-7.6.
//!
//! Everything below `adjust` works per QMF subband `m = k - kx`
//! (`0..m_max`) and per QMF slot, as the spec does: envelope/noise data is
//! mapped onto subbands first (`E_origMapped`, `Q_mapped`, `S_mapped`,
//! `S_indexMapped`), gains are computed per subband, the limiter works on
//! subband sums per limiter band, and the smoothing filter runs over the
//! previous four QMF SLOTS' gain vectors (carried across frames).
//!
//! Slot indexing: `hf[m][i]` is `X_high(k, i + tHFAdj)`, i.e. slot `i` is the
//! one envelope border `2*t_E` counts -- the caller supplies a buffer long
//! enough for `2*t_E(L_E)` (up to 38 slots: 32 of this frame plus the
//! 6-slot overlap the variable frame classes may reach into).
#![allow(clippy::needless_range_loop)]

use crate::sbr_bands::BandTables;
use crate::sbr_payload::{SbrChannel, SbrHeader};
use crate::sbr_tables::SBR_NOISE;
use ec_dsp::Complex;

const NOISE_FLOOR_OFFSET: f64 = 6.0;

/// Envelope amp-resolution exponent: `2.0` (`bs_amp_res=1`, 3 dB) or `1.0`
/// (`bs_amp_res=0`, 1.5 dB) -- expressed as the multiplier on `e_q/2` so
/// callers can write `2f64.powf(alpha * e_q as f64 / 2.0)` uniformly, or
/// just use [`dequant_env`] directly.
fn alpha(amp_res: u8) -> f64 {
    if amp_res != 0 { 1.0 } else { 0.5 }
}

/// Plain (non-coupled) envelope dequantization: `E_Orig = 2^(E_Q * a) * 64`
/// (§4.6.18.7.2), in the spec 32-band analysis domain `sbr_qmf::Analysis`
/// now produces directly.
pub fn dequant_env(e_q: i32, amp_res: u8) -> f64 {
    2f64.powf(alpha(amp_res) * f64::from(e_q)) * 64.0
}

/// Plain noise-floor dequantization: `Q_Orig = 2^(NOISE_FLOOR_OFFSET - Q_Q)`,
/// a ratio against the same cell's envelope energy (§4.6.18.7.2).
pub fn dequant_noise(q_q: i32) -> f64 {
    2f64.powf(NOISE_FLOOR_OFFSET - f64::from(q_q))
}

/// De-mixes a coupled CPE's `(channel 0 raw, balance raw)` pair into
/// `(env0, env1)` linear energies (ISO/IEC 14496-3 §4.6.18.7.2,
/// `bs_coupling`): `E = 2^(a*e0_raw + 7)`, `r = 2^(a*(panOffset - 2*pan))`
/// with `a` = 1 (3 dB) or 0.5 (1.5 dB) and `panOffset` = 12 / 24 for the
/// FRAME's amp_res (the FIXFIX single-envelope override included);
/// `env0 = E/(1+r)`, `env1 = env0*r`. The balance channel's raw values
/// (and its Huffman deltas) count in DOUBLE steps -- `bs_data_env` is one
/// bit narrower than the level field -- so its centre is 6 (3 dB) / 12
/// (1.5 dB) in raw units; measured on a real coupled stream, where 88% of
/// the raw balance values sit exactly on that centre.
pub fn dequant_pair(e0_raw: i32, pan_raw: i32, amp_res: u8) -> (f64, f64) {
    let a = alpha(amp_res);
    let pan_offset = if amp_res != 0 { 12.0 } else { 24.0 };
    let ratio = 2f64.powf(a * (pan_offset - 2.0 * f64::from(pan_raw)));
    let env0 = 2.0 * dequant_env(e0_raw, amp_res) / (1.0 + ratio);
    (env0, env0 * ratio)
}

/// Coupled noise-floor pair (§4.6.18.7.2): `Q0 = 2^(NOISE_FLOOR_OFFSET - q0 + 1)
/// / (1 + 2^(12 - 2*q1))`, `Q1 = Q0 * 2^(12 - 2*q1)` -- the raw balance
/// value counts in double steps, as for the envelope.
pub fn dequant_noise_pair(q0_raw: i32, pan_raw: i32) -> (f64, f64) {
    let temp1 = 2f64.powf(NOISE_FLOOR_OFFSET - f64::from(q0_raw) + 1.0);
    let temp2 = 2f64.powf(12.0 - 2.0 * f64::from(pan_raw));
    let q0 = temp1 / (1.0 + temp2);
    (q0, q0 * temp2)
}

/// Dequantizes one channel's envelopes and noise floors for one frame,
/// de-mixing through [`dequant_pair`]/[`dequant_noise_pair`] when
/// `coupling` makes `channels[1]` a balance channel against `channels[0]`.
pub fn dequantize_frame(
    channels: &[SbrChannel],
    ch: usize,
    coupling: bool,
) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let c = &channels[ch];
    let amp_res = c.amp_res;
    let coupled = coupling && channels.len() == 2;
    let pick = |pair: (f64, f64)| if ch == 0 { pair.0 } else { pair.1 };
    let env = if coupled {
        (0..c.e_q.len())
            .map(|i| {
                (0..c.e_q[i].len())
                    .map(|b| {
                        let e0 = channels[0].e_q.get(i).and_then(|r| r.get(b)).copied().unwrap_or(0);
                        let pan = channels[1].e_q.get(i).and_then(|r| r.get(b)).copied().unwrap_or(0);
                        pick(dequant_pair(e0, pan, amp_res))
                    })
                    .collect()
            })
            .collect()
    } else {
        c.e_q.iter().map(|row| row.iter().map(|&v| dequant_env(v, amp_res)).collect()).collect()
    };
    let noise = if coupled {
        (0..c.q_q.len())
            .map(|i| {
                (0..c.q_q[i].len())
                    .map(|b| {
                        let q0 = channels[0].q_q.get(i).and_then(|r| r.get(b)).copied().unwrap_or(0);
                        let pan = channels[1].q_q.get(i).and_then(|r| r.get(b)).copied().unwrap_or(0);
                        pick(dequant_noise_pair(q0, pan))
                    })
                    .collect()
            })
            .collect()
    } else {
        c.q_q.iter().map(|row| row.iter().map(|&v| dequant_noise(v)).collect()).collect()
    };
    (env, noise)
}

/// Limiter gain ceilings, `bs_limiter_gains` 0..3 (§4.6.18.7.5, Table 4.150):
/// -3 dB, 0 dB, +3 dB, off.
const LIMITER_FACTOR: [f64; 4] = [0.70795, 1.0, 1.41254, 1.0e10];
const GAIN_MAX_CLIP: f64 = 1.0e5;
const BOOST_MAX: f64 = 1.584_893_192;
const H_SMOOTH: [f64; 5] = [
    0.333_333_333_333_33,
    0.301_502_832_395_82,
    0.218_169_499_062_49,
    0.115_163_834_270_84,
    0.031_830_500_937_51,
];
const H_SL: usize = 4;
const EPS: f64 = 1.192_092_9e-7;

/// Per-channel adjuster state carried across frames (§4.6.18.7.6's
/// `G_Temp`/`Q_Temp` slot history, `f_IndexNoise`, `f_IndexSine`, the
/// previous frame's `S_indexMapped` and its `l_A`).
#[derive(Clone, Debug)]
pub struct AdjustState {
    s_index_prev: Vec<u8>,
    /// Gain/noise vectors of the last `H_SL` slots, oldest first.
    g_hist: std::collections::VecDeque<Vec<f64>>,
    q_hist: std::collections::VecDeque<Vec<f64>>,
    index_noise: usize,
    index_sine: usize,
    /// Previous frame's `l_A == L_E` (the transient envelope continues into
    /// this frame's envelope 0 -- `l_Aprev`).
    la_prev_at_end: bool,
    pub fresh: bool,
}

impl AdjustState {
    pub fn new() -> AdjustState {
        AdjustState {
            s_index_prev: Vec::new(),
            g_hist: std::collections::VecDeque::new(),
            q_hist: std::collections::VecDeque::new(),
            index_noise: 0,
            index_sine: 0,
            la_prev_at_end: false,
            fresh: true,
        }
    }
}

impl Default for AdjustState {
    fn default() -> Self {
        Self::new()
    }
}

fn noise_fraction_debug() -> bool {
    std::env::var("EC_AAC_SBR_NOISE_FRACTION").is_ok()
}

type NoiseFractionRows = Vec<(usize, f64, f64, f64)>;

static NOISE_FRACTION: std::sync::OnceLock<std::sync::Mutex<NoiseFractionRows>> =
    std::sync::OnceLock::new();

/// Diagnostic (`EC_AAC_SBR_NOISE_FRACTION`): per absolute QMF band,
/// bookkept `(band, Σ signal energy·slots, Σ noise energy·slots, Σ |Y|²
/// post-adjust)` -- the last column against the first two's sum is the
/// per-subband realised/target energy ratio.
pub fn noise_fraction_table() -> Vec<(usize, f64, f64, f64)> {
    let mut out: Vec<(usize, f64, f64, f64)> = NOISE_FRACTION
        .get()
        .and_then(|m| m.lock().ok())
        .map(|g| g.clone())
        .unwrap_or_default();
    out.sort_by_key(|e| e.0);
    out
}

/// Adjusts `hf` (`[m][slot]`, `m = k - kx`) in place: envelope gains,
/// noise floor and added sinusoids per §4.6.18.7.3-7.6. `ch.t_env`/`t_noise`
/// are in QMF slots (already ×2). `reset` mirrors the spec's header-reset
/// flag (smoothing history primed from the first envelope, indices zeroed).
#[allow(clippy::too_many_arguments)]
pub fn adjust(
    hf: &mut [Vec<Complex<f64>>],
    tables: &BandTables,
    header: &SbrHeader,
    ch: &SbrChannel,
    env_energy: &[Vec<f64>],
    noise_energy: &[Vec<f64>],
    limiter_table: &[i64],
    state: &mut AdjustState,
    reset: bool,
) {
    let kx = tables.kx as usize;
    let m_max = hf.len();
    let num_env = ch.t_env.len().saturating_sub(1).min(env_energy.len());
    if m_max == 0 || num_env == 0 {
        return;
    }
    let reset = reset || state.fresh || state.s_index_prev.len() != m_max;
    state.fresh = false;
    if reset {
        state.s_index_prev = vec![0; m_max];
        state.index_noise = 0;
        state.index_sine = 0;
        state.g_hist.clear();
        state.q_hist.clear();
    }
    let limiter_max = if std::env::var("EC_AAC_SBR_LIMITER_OFF").is_ok() {
        LIMITER_FACTOR[3]
    } else {
        LIMITER_FACTOR[usize::from(header.limiter_gains).min(3)]
    };
    let zero_noise = std::env::var("EC_AAC_SBR_NOISE_ZERO").is_ok();
    let limdump = std::env::var("EC_AAC_SBR_CELLDUMP").is_ok();
    let track_fraction = noise_fraction_debug();
    let h_sl = if header.smoothing_mode == 0 { H_SL } else { 0 };
    let l_a = ch.l_a;
    let l_a_prev: i64 = if state.la_prev_at_end { 0 } else { -1 };
    let n_lim = limiter_table.len().saturating_sub(1);

    // §4.6.18.7.2 mapping + §4.6.18.7.3 E_curr + §4.6.18.7.5 gains, per env.
    let mut gain = vec![vec![0.0f64; m_max]; num_env];
    let mut q_m = vec![vec![0.0f64; m_max]; num_env];
    let mut s_m = vec![vec![0.0f64; m_max]; num_env];
    let mut s_index_prev = state.s_index_prev.clone();
    for e in 0..num_env {
        let (t0, t1) = (ch.t_env[e].max(0) as usize, ch.t_env[e + 1].max(0) as usize);
        let high_res = ch.freq_res.get(e).copied().unwrap_or(0) != 0;
        let table: &[i64] = if high_res { &tables.f_high } else { &tables.f_low };
        let n_bands = table.len().saturating_sub(1);
        // E_origMapped
        let mut e_orig = vec![0.0f64; m_max];
        for i in 0..n_bands {
            let v = env_energy[e].get(i).copied().unwrap_or(0.0);
            for k in table[i]..table[i + 1] {
                let m = k as usize - kx.min(k as usize);
                if k as usize >= kx && m < m_max {
                    e_orig[m] = v;
                }
            }
        }
        // Q_mapped: noise floor covering this envelope's start.
        let nf = usize::from(ch.t_noise.len() > 2 && ch.t_env[e] >= ch.t_noise[1]);
        let mut q_mapped = vec![0.0f64; m_max];
        if let Some(row) = noise_energy.get(nf.min(noise_energy.len().saturating_sub(1))) {
            for i in 0..tables.n_q.min(tables.f_noise.len().saturating_sub(1)) {
                let v = row.get(i).copied().unwrap_or(0.0);
                for k in tables.f_noise[i]..tables.f_noise[i + 1] {
                    if k as usize >= kx && (k as usize - kx) < m_max {
                        q_mapped[k as usize - kx] = v;
                    }
                }
            }
        }
        // S_indexMapped (centre subband of each flagged high-res band, from
        // l_A on, or carried over from the previous frame) and S_mapped.
        let mut s_index = vec![0u8; m_max];
        if let Some(flags) = &ch.add_harmonic {
            for i in 0..tables.f_high.len().saturating_sub(1) {
                let mid = ((tables.f_high[i] + tables.f_high[i + 1]) >> 1) as usize;
                if mid < kx || mid - kx >= m_max {
                    continue;
                }
                let on = flags.get(i).copied().unwrap_or(0) != 0;
                s_index[mid - kx] =
                    u8::from(on && ((e as i64) >= l_a || s_index_prev[mid - kx] == 1));
            }
        }
        let mut s_mapped = vec![false; m_max];
        for i in 0..n_bands {
            let lo = (table[i] as usize).max(kx) - kx;
            let hi = ((table[i + 1] as usize).max(kx) - kx).min(m_max);
            let any = (lo..hi).any(|m| s_index[m] != 0);
            for m in lo..hi {
                s_mapped[m] = any;
            }
        }
        s_index_prev = s_index.clone();
        // E_curr (§4.6.18.7.3), bs_interpol_freq 1 per subband / 0 per band.
        let mut e_curr = vec![0.0f64; m_max];
        let slots = (t1.saturating_sub(t0)).max(1) as f64;
        if header.interpol_freq != 0 {
            for m in 0..m_max {
                let row = &hf[m];
                let sum: f64 =
                    (t0..t1.min(row.len())).map(|i| row[i].norm_sqr()).sum();
                e_curr[m] = sum / slots;
            }
        } else {
            for i in 0..n_bands {
                let lo = (table[i] as usize).max(kx) - kx;
                let hi = ((table[i + 1] as usize).max(kx) - kx).min(m_max);
                let mut sum = 0.0;
                for m in lo..hi {
                    let row = &hf[m];
                    sum += (t0..t1.min(row.len())).map(|i| row[i].norm_sqr()).sum::<f64>();
                }
                let v = sum / (slots * (hi.saturating_sub(lo)).max(1) as f64);
                for m in lo..hi {
                    e_curr[m] = v;
                }
            }
        }
        // Gains, limiter and boost (§4.6.18.7.5), per limiter band.
        let delta = !((e as i64) == l_a || (e as i64) == l_a_prev);
        let deltaf = if delta { 1.0 } else { 0.0 };
        for j in 0..n_lim {
            let lo = (limiter_table[j] as usize).max(kx) - kx;
            let hi = ((limiter_table[j + 1] as usize).max(kx) - kx).min(m_max);
            if lo >= hi {
                continue;
            }
            for m in lo..hi {
                let temp = e_orig[m] / (1.0 + q_mapped[m]);
                q_m[e][m] = (temp * q_mapped[m]).sqrt();
                s_m[e][m] = (temp * f64::from(s_index[m])).sqrt();
                gain[e][m] = if !s_mapped[m] {
                    (e_orig[m] / ((1.0 + e_curr[m]) * (1.0 + q_mapped[m] * deltaf))).sqrt()
                } else {
                    (e_orig[m] * q_mapped[m] / ((1.0 + e_curr[m]) * (1.0 + q_mapped[m]))).sqrt()
                } + f64::MIN_POSITIVE;
            }
            let sum_orig: f64 = (lo..hi).map(|m| e_orig[m]).sum();
            let sum_curr: f64 = (lo..hi).map(|m| e_curr[m]).sum();
            let g_max = (limiter_max * ((EPS + sum_orig) / (EPS + sum_curr)).sqrt()).min(GAIN_MAX_CLIP);
            let mut capped = 0usize;
            for m in lo..hi {
                let q_max = q_m[e][m] * g_max / gain[e][m];
                q_m[e][m] = q_m[e][m].min(q_max);
                if gain[e][m] > g_max {
                    capped += 1;
                }
                gain[e][m] = gain[e][m].min(g_max);
            }
            let sum_act: f64 = (lo..hi)
                .map(|m| {
                    e_curr[m] * gain[e][m] * gain[e][m]
                        + s_m[e][m] * s_m[e][m]
                        + if delta && s_m[e][m] == 0.0 { q_m[e][m] * q_m[e][m] } else { 0.0 }
                })
                .sum();
            let boost = ((EPS + sum_orig) / (EPS + sum_act)).sqrt().min(BOOST_MAX);
            for m in lo..hi {
                gain[e][m] *= boost;
                q_m[e][m] *= boost;
                s_m[e][m] *= boost;
            }
            if limdump {
                eprintln!(
                    "LIMDUMP ei={e} band=[{},{}) limiter_bands={} j={j}/{n_lim} \
                     E_orig_mapped={sum_orig:.6e} E_curr={sum_curr:.6e} G_max={g_max:.6} \
                     capped_cells={capped}/{} E_actual={sum_act:.6e} G_boost={boost:.6}",
                    limiter_table[j],
                    limiter_table[j + 1],
                    header.limiter_bands,
                    hi - lo,
                );
            }
        }
        if track_fraction {
            let tab = NOISE_FRACTION.get_or_init(|| std::sync::Mutex::new(Vec::new()));
            if let Ok(mut t) = tab.lock() {
                for m in 0..m_max {
                    let sig = e_orig[m] / (1.0 + q_mapped[m]) * slots;
                    let noi = e_orig[m] * q_mapped[m] / (1.0 + q_mapped[m]) * slots;
                    if let Some(row) = t.iter_mut().find(|r| r.0 == m + kx) {
                        row.1 += sig;
                        row.2 += noi;
                    } else {
                        t.push((m + kx, sig, noi, 0.0));
                    }
                }
            }
        }
    }
    state.s_index_prev = s_index_prev;
    state.la_prev_at_end = l_a == num_env as i64;

    // §4.6.18.7.6 assembly: smoothing over the previous H_SL slots, noise
    // from the V table, sinusoids with the (-1)^k phase alternation.
    if reset || state.g_hist.len() != h_sl {
        state.g_hist.clear();
        state.q_hist.clear();
        for _ in 0..h_sl {
            state.g_hist.push_back(gain[0].clone());
            state.q_hist.push_back(q_m[0].clone());
        }
    }
    let phi_sign = if kx & 1 == 1 { -1.0 } else { 1.0 };
    let mut g_filt = vec![0.0f64; m_max];
    let mut q_filt = vec![0.0f64; m_max];
    for e in 0..num_env {
        let (t0, t1) = (ch.t_env[e].max(0) as usize, ch.t_env[e + 1].max(0) as usize);
        let transient = (e as i64) == l_a || (e as i64) == l_a_prev;
        for i in t0..t1 {
            if h_sl > 0 && !transient {
                for m in 0..m_max {
                    let mut g = gain[e][m] * H_SMOOTH[0];
                    let mut q = q_m[e][m] * H_SMOOTH[0];
                    for j in 1..=h_sl {
                        // g_hist is oldest-first; j=1 is the most recent slot.
                        g += state.g_hist[h_sl - j][m] * H_SMOOTH[j];
                        q += state.q_hist[h_sl - j][m] * H_SMOOTH[j];
                    }
                    g_filt[m] = g;
                    q_filt[m] = q;
                }
            } else {
                g_filt.copy_from_slice(&gain[e]);
                q_filt.copy_from_slice(&q_m[e]);
            }
            if h_sl > 0 {
                state.g_hist.pop_front();
                state.q_hist.pop_front();
                state.g_hist.push_back(gain[e].clone());
                state.q_hist.push_back(q_m[e].clone());
            }
            // Sinusoid phase for this slot: f_IndexSine 0..3 ->
            // (1,0), (0,phi), (-1,0), (0,-phi), phi alternating per subband.
            let (p_re, p_im0) = match state.index_sine {
                0 => (1.0, 0.0),
                1 => (0.0, phi_sign),
                2 => (-1.0, 0.0),
                _ => (0.0, -phi_sign),
            };
            let mut p_im = p_im0;
            let mut noise_idx = state.index_noise;
            for m in 0..m_max {
                let row = &mut hf[m];
                if i >= row.len() {
                    break;
                }
                let mut y = row[i].scale(g_filt[m]);
                noise_idx = (noise_idx + 1) & 0x1ff;
                if s_m[e][m] != 0.0 {
                    y = y + Complex::new(s_m[e][m] * p_re, s_m[e][m] * p_im);
                } else if !transient && !zero_noise {
                    let (vr, vi) = SBR_NOISE[noise_idx];
                    y = y + Complex::new(q_filt[m] * vr, q_filt[m] * vi);
                }
                row[i] = y;
                p_im = -p_im;
            }
            if track_fraction {
                let tab = NOISE_FRACTION.get_or_init(|| std::sync::Mutex::new(Vec::new()));
                if let Ok(mut t) = tab.lock() {
                    for m in 0..m_max {
                        if let (Some(row), Some(y)) =
                            (t.iter_mut().find(|r| r.0 == m + kx), hf[m].get(i))
                        {
                            row.3 += y.norm_sqr();
                        }
                    }
                }
            }
            state.index_noise = (state.index_noise + m_max) & 0x1ff;
            state.index_sine = (state.index_sine + 1) & 3;
        }
    }
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

    fn channel(tables: &BandTables, add_harmonic: Option<Vec<u8>>) -> SbrChannel {
        SbrChannel {
            t_env: vec![0, 16],
            freq_res: vec![1],
            t_noise: vec![0, 16],
            e_q: vec![vec![0; tables.n_high]],
            q_q: vec![vec![0; tables.n_q]],
            invf_mode: vec![],
            add_harmonic,
            df_env: vec![],
            df_noise: vec![],
            l_a: -1,
            amp_res: 1,
        }
    }

    #[test]
    fn flat_hf_region_reaches_the_stated_envelope_energy() {
        let tables = flat_tables();
        let (kx, k2) = (tables.kx as usize, tables.k2 as usize);
        let slots = 16usize;
        let mut hf = vec![vec![Complex::new(0.1, 0.0); slots]; k2 - kx];
        let target = 4.0f64;
        let env_energy = vec![vec![target; tables.n_high]];
        // Q_mapped = 1 (q_q = 6): half the target is noise.
        let noise_energy = vec![vec![1.0f64; tables.n_q]];
        let hdr = header(1, 3);
        let lim = limiter_band_table(&tables, &build_patches(&tables), hdr.limiter_bands);
        let mut st = AdjustState::new();
        adjust(&mut hf, &tables, &hdr, &channel(&tables, None), &env_energy, &noise_energy, &lim, &mut st, true);
        for m in 0..k2 - kx {
            let avg: f64 = hf[m].iter().map(|c| c.norm_sqr()).sum::<f64>() / slots as f64;
            // signal share exactly target/2 (flat input, G cancels E_curr);
            // noise share target/2 in expectation over the V table.
            assert!((avg - target).abs() / target < 0.4, "m={m}: avg {avg} vs target {target}");
        }
    }

    #[test]
    fn sinusoid_lands_on_the_centre_subband_only_and_mutes_noise_there() {
        let tables = flat_tables();
        let (kx, k2) = (tables.kx as usize, tables.k2 as usize);
        let slots = 16usize;
        let mut hf = vec![vec![Complex::ZERO; slots]; k2 - kx];
        let mut flags = vec![0u8; tables.n_high];
        flags[1] = 1;
        let env_energy = vec![vec![4.0f64; tables.n_high]];
        let noise_energy = vec![vec![0.0f64; tables.n_q]];
        let hdr = header(1, 1);
        let lim = limiter_band_table(&tables, &build_patches(&tables), hdr.limiter_bands);
        let mut st = AdjustState::new();
        adjust(&mut hf, &tables, &hdr, &channel(&tables, Some(flags)), &env_energy, &noise_energy, &lim, &mut st, true);
        let mid = ((tables.f_high[1] + tables.f_high[2]) >> 1) as usize - kx;
        for m in 0..k2 - kx {
            let e: f64 = hf[m].iter().map(|c| c.norm_sqr()).sum::<f64>() / slots as f64;
            if m == mid {
                // S_M^2 = E_orig/(1+Q) = 4, boost capped at 1.5849^2 at most.
                assert!((4.0 - 1e-9..=4.0 * BOOST_MAX * BOOST_MAX + 1e-6).contains(&e), "mid {e}");
                // Phase walks 0,90,180,270 degrees per slot.
                assert!((hf[m][0].re - hf[m][0].norm_sqr().sqrt()).abs() < 1e-9 && hf[m][1].re.abs() < 1e-9);
            } else {
                assert!(e < 1e-9, "m={m}: {e}");
            }
        }
    }

    #[test]
    fn coupled_balance_round_trips_at_a_centred_ratio() {
        let (e0, e1) = dequant_pair(10, 6, 1);
        let e0_plain = dequant_env(10, 1);
        assert!((e0 - e0_plain).abs() < 1e-6 && (e1 - e0_plain).abs() < 1e-6, "{e0} {e1} vs {e0_plain}");
        let (e00, e10) = dequant_pair(10, 12, 0);
        let e00_plain = dequant_env(10, 0);
        assert!((e00 - e00_plain).abs() < 1e-6 && (e10 - e00_plain).abs() < 1e-6, "{e00} {e10} vs {e00_plain}");
        let (q0, q1) = dequant_noise_pair(3, 6);
        assert!((q0 - dequant_noise(3)).abs() < 1e-9 && (q1 - q0).abs() < 1e-9);
    }

    #[test]
    fn coupled_balance_shifts_energy_toward_channel_0_with_a_higher_raw_value() {
        let (e0_low, e1_low) = dequant_pair(10, 0, 1);
        let (e0_high, e1_high) = dequant_pair(10, 12, 1);
        assert!(e0_low < e1_low, "{e0_low} vs {e1_low}");
        assert!(e0_high > e1_high, "{e0_high} vs {e1_high}");
        // One raw balance step is two amp_res steps of ratio: 6 dB at
        // amp_res 1, 3 dB at amp_res 0.
        let (a0, a1) = dequant_pair(10, 5, 1);
        let (b0, b1) = dequant_pair(10, 11, 0);
        assert!((a1 / a0 - 4.0).abs() < 1e-9 && (b1 / b0 - 2.0).abs() < 1e-9, "{a0} {a1} {b0} {b1}");
    }
}

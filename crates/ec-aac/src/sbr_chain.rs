//! Wires the SBR payload parser, HF generator and envelope adjuster into
//! one per-element chain the decoder's `raw_data_block` loop drives: parse
//! a FIL element's `sbr_extension_data`, run the previous SCE/CPE's core
//! PCM through 32-band analysis, generate and adjust the HF region, and
//! resynthesize through the 64-band bank to double-rate PCM in place.

use crate::sbr_env::{self, AdjustState};
use crate::sbr_hf::{self, ChirpState, HfHistory};
use crate::sbr_payload::{SbrChannel, SbrData, SbrParser};
use crate::sbr_qmf::{ANALYSIS_BANDS, Analysis, SYNTHESIS_BANDS, Synthesis};
use ec_core::BitReader;
use ec_dsp::Complex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Debug: dumps parsed grid/header/e_q/q_q values for the first 20 frames
/// per channel when `EC_AAC_SBR_DEBUG` is set (diagnostic only, gated).
fn debug_dump(
    header: &crate::sbr_payload::SbrHeader,
    tables: &crate::sbr_bands::BandTables,
    ch: &SbrChannel,
    ch_idx: usize,
) {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if n >= 20 {
        return;
    }
    eprintln!(
        "SBRDBG frame {n} ch{ch_idx}: amp_res={} kx={} k2={} n_low={} n_high={} n_q={} t_env={:?} freq_res={:?} t_noise={:?} f_high={:?} patches={:?}",
        header.amp_res,
        tables.kx,
        tables.k2,
        tables.n_low,
        tables.n_high,
        tables.n_q,
        ch.t_env,
        ch.freq_res,
        ch.t_noise,
        tables.f_high,
        crate::sbr_hf::build_patches(tables),
    );
    for (i, row) in ch.e_q.iter().enumerate() {
        let (mn, mx) = row
            .iter()
            .fold((i32::MAX, i32::MIN), |(mn, mx), &v| (mn.min(v), mx.max(v)));
        eprintln!("  e_q[{i}] len={} min={mn} max={mx} row={row:?}", row.len());
    }
    for (i, row) in ch.q_q.iter().enumerate() {
        let (mn, mx) = row
            .iter()
            .fold((i32::MAX, i32::MIN), |(mn, mx), &v| (mn.min(v), mx.max(v)));
        eprintln!("  q_q[{i}] len={} min={mn} max={mx} row={row:?}", row.len());
    }
}

/// Envelope-grid "time slots" (§4.6.18.4, `numTimeSlots = 16` for a
/// 1024-sample core frame) are half as fine as the QMF slots SBR actually
/// runs at (32 per frame): `RATE = 2` converts one to the other.
const RATE: i64 = 2;

/// Round-20 residual/side-info correlation: one row per (access unit,
/// channel) `apply_data` actually ran, giving `sbr_real_library.rs`'s
/// residual-energy grid a side-info table to correlate against without
/// re-parsing debug text. `frame` is a single counter shared by every
/// channel `apply_data` touches in one call, so it lines up 1:1 with the
/// AU-aligned (2048 output samples/AU for a plain 2x stream) blocks the
/// residual grid uses.
#[derive(Clone, Debug)]
pub struct SbrSideInfoRow {
    pub frame: usize,
    pub tag: u8,
    pub ch: usize,
    /// `"fresh"` for a frame `apply` reached (a real payload this AU),
    /// `"hold"` for one `apply_last` reached (no fresh payload, reusing the
    /// last parsed frame's data per §4.6.18).
    pub source: &'static str,
    pub coupling: bool,
    pub amp_res: u8,
    /// Band-index domain (same units `f_high`/`f_noise` and the residual
    /// grid's FFT-bucket index share, per `per_band_correlation`'s
    /// `band_hz = rate/256` measurement) the crossover and high-res/noise
    /// band boundaries below live in.
    pub kx: i64,
    pub k2: i64,
    pub f_high: Vec<i64>,
    pub f_noise: Vec<i64>,
    pub t_env: Vec<i64>,
    pub freq_res: Vec<u8>,
    pub invf_mode: Vec<u8>,
    pub add_harmonic: Option<Vec<u8>>,
    pub e_q_means: Vec<f64>,
    pub q_q_means: Vec<f64>,
    /// Header fields held for the frame's duration (§4.6.18.3.2), added for
    /// the FMJ-vs-Nikbinler feature-table hunt: `sbr_header()`'s own coding
    /// choices, not per-envelope grid data.
    pub freq_scale: u8,
    pub alter_scale: u8,
    pub noise_bands: u8,
    pub limiter_bands: u8,
    pub limiter_gains: u8,
    pub interpol_freq: u8,
    pub smoothing_mode: u8,
    pub start_freq: u8,
    pub stop_freq: u8,
    pub xover_band: u8,
    /// `t_noise.len() - 1`, i.e. `numNoiseFloors` this frame.
    pub num_noise: usize,
    /// One entry per HF patch, each the patch's band count (from
    /// `sbr_hf::build_patches`), so patch geometry is visible per frame.
    pub patch_lengths: Vec<usize>,
    pub df_env: Vec<u8>,
    pub df_noise: Vec<u8>,
}

static SIDEINFO_LOG: std::sync::OnceLock<std::sync::Mutex<Vec<SbrSideInfoRow>>> =
    std::sync::OnceLock::new();
static SIDEINFO_FRAME: AtomicUsize = AtomicUsize::new(0);

/// Diagnostic-only counter: how many `apply_data` calls process-wide ran
/// with `source == "hold"` (an AU whose FIL carried no fresh SBR payload,
/// falling back to `apply_last`'s reuse of the last parsed frame). Read via
/// [`hold_call_count`]; not gated by an env var since incrementing an atomic
/// is unconditionally cheap, unlike the `EC_AAC_SBR_*_DEBUG` prints nearby.
static HOLD_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Total `apply_data` calls process-wide where the SBR payload was reused
/// from a prior frame (`apply_last`) rather than freshly parsed this AU --
/// diagnostic instrument for `sbr441_family_sample_drift_probe`.
pub fn hold_call_count() -> usize {
    HOLD_CALLS.load(Ordering::Relaxed)
}

fn sideinfo_enabled() -> bool {
    std::env::var("EC_AAC_SBR_SIDEINFO_DEBUG").is_ok()
}

/// Round-34 Task 1 diagnostic: per-AU per-QMF-band mean energy at a named
/// pipeline stage, gated by `EC_AAC_SBR_QMFDUMP`. `start_band` is the
/// absolute QMF band index `rows[0]` represents (`0` for the core analysis
/// matrix, `kx` for the HF matrix), so every printed band index lines up
/// with `build_patches`' target-band units without the reader having to
/// offset anything by hand.
fn qmfdump_enabled() -> bool {
    std::env::var("EC_AAC_SBR_QMFDUMP").is_ok()
}

fn qmfdump_energies(stage: &str, start_band: usize, rows: &[Vec<Complex<f64>>]) {
    let means: Vec<f64> = rows
        .iter()
        .map(|series| {
            if series.is_empty() {
                0.0
            } else {
                series
                    .iter()
                    .map(|c| c.re * c.re + c.im * c.im)
                    .sum::<f64>()
                    / series.len() as f64
            }
        })
        .collect();
    eprintln!("QMFDUMP stage={stage} start_band={start_band} means={means:?}");
}

/// Drains nothing -- returns a clone of every row logged so far this
/// process, in call order. Empty unless `EC_AAC_SBR_SIDEINFO_DEBUG` was set
/// before decoding.
pub fn sbr_sideinfo_log() -> Vec<SbrSideInfoRow> {
    let Some(log) = SIDEINFO_LOG.get() else {
        return Vec::new();
    };
    log.lock().map(|l| l.clone()).unwrap_or_default()
}

/// Per-channel DSP state an SBR element's reconstruction carries across
/// frames: the QMF filterbank pair, the HF generator's LPC history and
/// chirp factors, and the noise-floor generator.
struct ChannelState {
    analysis: Analysis,
    synthesis: Synthesis,
    hf_hist: HfHistory,
    chirp: ChirpState,
    adj: AdjustState,
    /// §4.6.18.7.6 `tHFAdj`/overlap buffers: the SBR output lags the raw
    /// analysis by `HF_ADJ` slots, so the previous frame's last `HF_ADJ`
    /// raw low-band and raw HF slots are kept, plus the already-adjusted HF
    /// slots past the frame end that a variable-border envelope produced
    /// (`Y` of slots `32..2*t_E(L_E)`), which lead the next frame's output.
    low_tail: Vec<[Complex<f64>; ANALYSIS_BANDS]>,
    hf_tail: Vec<Vec<Complex<f64>>>,
    y_carry: Vec<Vec<Complex<f64>>>,
}

/// Slots by which the HF adjuster's envelope grid lags the raw QMF frame
/// (`tHFAdj` + the 6-slot overlap, §4.6.18.7.6 / Figure 4.32).
const HF_ADJ: usize = 6;

impl ChannelState {
    fn new(n_q: usize, _seed: u32) -> ChannelState {
        ChannelState {
            analysis: Analysis::new(),
            synthesis: Synthesis::new(),
            hf_hist: HfHistory::new(ANALYSIS_BANDS),
            chirp: ChirpState::new(n_q.max(1)),
            adj: AdjustState::new(),
            low_tail: vec![[Complex::ZERO; ANALYSIS_BANDS]; HF_ADJ],
            hf_tail: Vec::new(),
            y_carry: Vec::new(),
        }
    }
}

/// One SBR element's (SCE or CPE) persistent state.
struct Element {
    parser: SbrParser,
    channels: Vec<ChannelState>,
    /// The most recently parsed frame's envelope/noise/grid data, held so an
    /// access unit whose FIL element carries no fresh SBR payload this frame
    /// (an "unavailable" frame, §4.6.18: legal per spec and the decoder must
    /// keep reconstructing at the doubled rate using the last known data,
    /// not silently fall back to core-only output) can still be
    /// SBR-reconstructed instead of leaving that one access unit's plane at
    /// half length -- which otherwise permanently shifts every sample after
    /// it out of alignment with a reference that stayed at full rate.
    last_data: Option<SbrData>,
    /// Header of the previous applied frame: a change is the spec's reset.
    last_header: Option<crate::sbr_payload::SbrHeader>,
}

/// Every configured SBR element in a stream, keyed by
/// `element_instance_tag` (assumed stable frame to frame, as every
/// encoder in practice emits it).
pub struct SbrChain {
    rate: u32,
    // Keyed by (element_instance_tag, is_cpe): SCE/LFE and CPE instance
    // tags are independent namespaces per §4.4 (an encoder may legally
    // reuse tag 0 for both an SCE and a CPE in the same access unit), so
    // a bare `u8` key aliased two distinct elements' QMF/HF/noise state
    // together whenever that happened -- exactly the case in a 5.1
    // stream (SCE + two CPEs).
    elements: HashMap<(u8, bool), Element>,
}

impl SbrChain {
    pub fn new(rate: u32) -> SbrChain {
        SbrChain {
            rate,
            elements: HashMap::new(),
        }
    }

    /// Parses one FIL element's `sbr_extension_data` (the reader
    /// positioned right past `bs_extension_type`) for element `tag`.
    pub fn parse(&mut self, r: &mut BitReader<'_>, tag: u8, is_cpe: bool) -> Option<SbrData> {
        let elem = self.elements.entry((tag, is_cpe)).or_insert_with(|| Element {
            parser: SbrParser::new(self.rate),
            channels: Vec::new(),
            last_data: None,
            last_header: None,
        });
        let data = elem.parser.parse(r, is_cpe).ok()?;
        let n_q = elem.parser.tables().map(|t| t.n_q).unwrap_or(1);
        while elem.channels.len() < data.channels.len() {
            let seed = 0x1234_5678u32.wrapping_add(elem.channels.len() as u32 * 0x9e37_79b9);
            elem.channels.push(ChannelState::new(n_q, seed));
        }
        Some(data)
    }

    /// Runs the chain over `planes` (element `tag`'s already-decoded core
    /// PCM, one plane per channel), replacing each in place with its
    /// SBR-reconstructed, double-rate PCM. A malformed/missing state is a
    /// silent no-op: `planes` is left at its core content.
    pub fn apply(&mut self, tag: u8, is_cpe: bool, data: &SbrData, planes: &mut [Vec<f32>]) {
        if let Some(elem) = self.elements.get_mut(&(tag, is_cpe)) {
            elem.last_data = Some(data.clone());
        }
        self.apply_data(tag, is_cpe, data, planes, "fresh");
    }

    /// Re-runs the chain for `tag` using the last successfully parsed
    /// frame's SBR data, for an access unit whose own FIL element carried no
    /// fresh payload this frame. A no-op (planes left at core content, same
    /// as [`apply`]'s existing malformed/missing-state fallback) if no prior
    /// frame ever parsed successfully for this element.
    pub fn apply_last(&mut self, tag: u8, is_cpe: bool, planes: &mut [Vec<f32>]) {
        let Some(data) = self
            .elements
            .get(&(tag, is_cpe))
            .and_then(|e| e.last_data.clone())
        else {
            return;
        };
        self.apply_data(tag, is_cpe, &data, planes, "hold");
    }

    fn apply_data(
        &mut self,
        tag: u8,
        is_cpe: bool,
        data: &SbrData,
        planes: &mut [Vec<f32>],
        source: &'static str,
    ) {
        if source == "hold" {
            HOLD_CALLS.fetch_add(1, Ordering::Relaxed);
        }
        let Some(elem) = self.elements.get_mut(&(tag, is_cpe)) else {
            return;
        };
        let Some(tables) = elem.parser.tables().cloned() else {
            return;
        };
        let reset = elem.last_header.as_ref() != elem.parser.header();
        elem.last_header = elem.parser.header().cloned();
        let Some(header) = elem.parser.header().cloned() else {
            return;
        };
        let coupling = data.coupling;
        let sideinfo_on = sideinfo_enabled();
        let au_frame = if sideinfo_on {
            SIDEINFO_FRAME.fetch_add(1, Ordering::Relaxed)
        } else {
            0
        };
        for (ch, plane) in planes.iter_mut().enumerate() {
            if ch >= data.channels.len() || ch >= elem.channels.len() {
                continue;
            }
            let num_slots = plane.len() / ANALYSIS_BANDS;
            if num_slots == 0 {
                continue;
            }
            if std::env::var("EC_AAC_SBR_PREQMF_DUMP").is_ok() {
                static N: AtomicUsize = AtomicUsize::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n < 40 {
                    let rms = (plane.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>()
                        / plane.len().max(1) as f64)
                        .sqrt();
                    eprintln!(
                        "PREQMF n={n} tag={tag} ch={ch} len={} rms={rms:.6} first8={:?}",
                        plane.len(),
                        &plane[..8.min(plane.len())]
                    );
                }
            }
            let state = &mut elem.channels[ch];
            let mut low_cur = vec![vec![Complex::ZERO; num_slots]; ANALYSIS_BANDS];
            let mut raw = Vec::with_capacity(num_slots);
            for slot in 0..num_slots {
                let mut chunk = [0f32; ANALYSIS_BANDS];
                chunk.copy_from_slice(&plane[slot * ANALYSIS_BANDS..(slot + 1) * ANALYSIS_BANDS]);
                // `plane` is already at the decoder's final output scale
                // (`decode::OUTPUT_SCALE`, applied by the core filterbank),
                // but the transmitted envelope/noise data is calibrated
                // against the core's un-normalized internal PCM domain --
                // undo that scale before analysis so the SBR gain match
                // (raw QMF energy vs. the transmitted target) compares
                // like against like, and reapply it once below after
                // synthesis. Without this the gain step's `target/current`
                // ratio is inflated by `1/OUTPUT_SCALE^2` in energy,
                // exactly the multi-order-of-magnitude blowup this chain
                // used to produce on every real HE-AAC file.
                for s in &mut chunk {
                    *s /= crate::decode::OUTPUT_SCALE;
                }
                let sub = state.analysis.process_slot(&chunk);
                for b in 0..ANALYSIS_BANDS {
                    low_cur[b][slot] = sub[b];
                }
                raw.push(sub);
            }

            let sbr_ch = &data.channels[ch];
            if std::env::var("EC_AAC_SBR_DEBUG").is_ok() {
                eprintln!("SBRDBG coupling={}", data.coupling);
                debug_dump(&header, &tables, sbr_ch, ch);
            }
            if sideinfo_on {
                let mean = |rows: &[Vec<i32>]| -> Vec<f64> {
                    rows.iter()
                        .map(|r| {
                            if r.is_empty() {
                                0.0
                            } else {
                                r.iter().map(|&v| f64::from(v)).sum::<f64>() / r.len() as f64
                            }
                        })
                        .collect()
                };
                let row = SbrSideInfoRow {
                    frame: au_frame,
                    tag,
                    ch,
                    source,
                    coupling,
                    amp_res: header.amp_res,
                    kx: tables.kx,
                    k2: tables.k2,
                    f_high: tables.f_high.clone(),
                    f_noise: tables.f_noise.clone(),
                    t_env: sbr_ch.t_env.clone(),
                    freq_res: sbr_ch.freq_res.clone(),
                    invf_mode: sbr_ch.invf_mode.clone(),
                    add_harmonic: sbr_ch.add_harmonic.clone(),
                    e_q_means: mean(&sbr_ch.e_q),
                    q_q_means: mean(&sbr_ch.q_q),
                    freq_scale: header.freq_scale,
                    alter_scale: header.alter_scale,
                    noise_bands: header.noise_bands,
                    limiter_bands: header.limiter_bands,
                    limiter_gains: header.limiter_gains,
                    interpol_freq: header.interpol_freq,
                    smoothing_mode: header.smoothing_mode,
                    start_freq: header.start_freq,
                    stop_freq: header.stop_freq,
                    xover_band: header.xover_band,
                    num_noise: sbr_ch.t_noise.len().saturating_sub(1),
                    patch_lengths: sbr_hf::build_patches(&tables).iter().map(|p| p.width).collect(),
                    df_env: sbr_ch.df_env.clone(),
                    df_noise: sbr_ch.df_noise.clone(),
                };
                let log = SIDEINFO_LOG.get_or_init(|| std::sync::Mutex::new(Vec::new()));
                if let Ok(mut l) = log.lock() {
                    l.push(row);
                }
            }
            let qmfdump_on = qmfdump_enabled();
            if qmfdump_on {
                qmfdump_energies("analysis", 0, &low_cur);
            }
            let hf = sbr_hf::generate(
                &low_cur,
                &tables,
                &sbr_ch.invf_mode,
                &mut state.chirp,
                &mut state.hf_hist,
            );
            if qmfdump_on {
                qmfdump_energies("post_patch", tables.kx as usize, &hf);
            }
            let (env_energy, noise_energy) =
                sbr_env::dequantize_frame(&header, &data.channels, ch, coupling);
            // sbr_env::adjust works in QMF-slot units; the parsed borders
            // are in the coarser envelope-time-slot units (see RATE above).
            let scaled = SbrChannel {
                t_env: sbr_ch.t_env.iter().map(|&t| t * RATE).collect(),
                t_noise: sbr_ch.t_noise.iter().map(|&t| t * RATE).collect(),
                ..sbr_ch.clone()
            };
            let limiter_table = sbr_hf::limiter_band_table(
                &tables,
                &sbr_hf::build_patches(&tables),
                header.limiter_bands,
            );
            // Envelope slot i is raw slot i - HF_ADJ: prepend the previous
            // frame's raw HF tail so the grid (up to 2*t_E(L_E) <= 38) lines
            // up with the content it was measured on (§4.6.18.7.6).
            let m_max = hf.len();
            if state.hf_tail.len() != m_max || reset {
                state.hf_tail = vec![vec![Complex::ZERO; HF_ADJ]; m_max];
                state.y_carry.clear();
            }
            let mut xh: Vec<Vec<Complex<f64>>> = (0..m_max)
                .map(|m| {
                    let mut v = state.hf_tail[m].clone();
                    v.extend_from_slice(&hf[m]);
                    v
                })
                .collect();
            for m in 0..m_max {
                state.hf_tail[m] = hf[m][num_slots.saturating_sub(HF_ADJ)..].to_vec();
            }
            sbr_env::adjust(
                &mut xh,
                &tables,
                &header,
                &scaled,
                &env_energy,
                &noise_energy,
                &limiter_table,
                &mut state.adj,
                reset,
            );
            // Output slot i: low band from the delayed raw analysis, HF from
            // the previous frame's carried-over adjusted slots first, then
            // this frame's; slots past the frame end are carried forward.
            let i_temp = state.y_carry.first().map(Vec::len).unwrap_or(0).min(num_slots);
            let t_end = (scaled.t_env.last().copied().unwrap_or(0).max(0) as usize).min(xh[0].len());
            let mut hf: Vec<Vec<Complex<f64>>> = vec![vec![Complex::ZERO; num_slots]; m_max];
            for m in 0..m_max {
                for i in 0..i_temp {
                    hf[m][i] = state.y_carry[m][i];
                }
                for i in i_temp..num_slots {
                    hf[m][i] = xh[m][i];
                }
            }
            state.y_carry = (0..m_max)
                .map(|m| xh[m][num_slots.min(t_end)..t_end].to_vec())
                .collect();
            let raw: Vec<[Complex<f64>; ANALYSIS_BANDS]> = {
                let mut d = state.low_tail.clone();
                d.extend_from_slice(&raw);
                state.low_tail = d[d.len() - HF_ADJ..].to_vec();
                d.truncate(num_slots);
                d
            };
            if qmfdump_on {
                qmfdump_energies("post_adjust", tables.kx as usize, &hf);
            }

            let kx = (tables.kx as usize).min(ANALYSIS_BANDS);
            let k2 = (tables.k2 as usize).min(SYNTHESIS_BANDS);
            let mut out = Vec::with_capacity(num_slots * SYNTHESIS_BANDS);
            for slot in 0..num_slots {
                let mut v = [Complex::ZERO; SYNTHESIS_BANDS];
                v[0..kx].copy_from_slice(&raw[slot][0..kx]);
                if std::env::var("EC_AAC_SBR_HF_BYPASS").is_err() {
                    for b in kx..k2 {
                        if b - kx < hf.len() {
                            v[b] = hf[b - kx][slot];
                        }
                    }
                }
                let mut pcm = state.synthesis.process_slot(&v);
                for s in &mut pcm {
                    // The spec 32-analysis/64-synthesis pair (4.6.18.4.1 +
                    // 4.6.18.8.2, literal equations) reads back at half the
                    // core's amplitude; the 2x is the upsampler's gain.
                    *s *= 2.0 * crate::decode::OUTPUT_SCALE;
                }
                out.extend_from_slice(&pcm);
            }
            if std::env::var("EC_AAC_SBR_PREQMF_DUMP").is_ok() {
                static N: AtomicUsize = AtomicUsize::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                if n < 40 {
                    let rms = (out.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>()
                        / out.len().max(1) as f64)
                        .sqrt();
                    eprintln!(
                        "POSTQMF n={n} tag={tag} ch={ch} len={} rms={rms:.6} first8={:?}",
                        out.len(),
                        &out[..8.min(out.len())]
                    );
                }
            }
            *plane = out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sbr_bands::freq_tables;
    use crate::sbr_payload::{SbrHeader, SbrParser};

    fn tables() -> crate::sbr_bands::BandTables {
        freq_tables(44100, 5, 3, 2, 1, 2, 2).unwrap()
    }

    fn header() -> SbrHeader {
        SbrHeader {
            amp_res: 1,
            start_freq: 5,
            stop_freq: 3,
            xover_band: 2,
            freq_scale: 2,
            alter_scale: 1,
            noise_bands: 2,
            limiter_bands: 2,
            limiter_gains: 3,
            interpol_freq: 1,
            smoothing_mode: 1,
        }
    }

    fn one_channel(n_q: usize, n_low: usize) -> SbrChannel {
        SbrChannel {
            t_env: vec![0, 16],
            freq_res: vec![0],
            t_noise: vec![0, 16],
            e_q: vec![vec![10i32; n_low]],
            q_q: vec![vec![2i32; n_q]],
            invf_mode: vec![0; n_q],
            add_harmonic: None,
            df_env: vec![],
            df_noise: vec![],
            l_a: -1,
            amp_res: 1,
        }
    }

    /// Two adjacent 1024-sample core AUs run through the same `SbrChain`
    /// element (as `raw_data_block`'s per-AU calls do) must each double to
    /// exactly `2 * ANALYSIS_BANDS * 32` samples with no gap or overlap at
    /// the junction: the coverage hole that let a whole access unit's
    /// doubling go missing (see `apply_last`) never showed up in the
    /// synthetic continuous-stream QMF round trip, since that test never
    /// crosses an AU boundary at all.
    #[test]
    fn two_adjacent_access_units_double_rate_with_no_junction_discontinuity() {
        let t = tables();
        let n_low = t.n_low;
        let n_q = t.n_q;
        let mut chain = SbrChain::new(44100);
        let tag = 0u8;
        chain.elements.insert(
            (tag, true),
            Element {
                parser: SbrParser::new(44100),
                channels: vec![ChannelState::new(n_q, 1), ChannelState::new(n_q, 2)],
                last_data: None,
            last_header: None,
            },
        );
        chain
            .elements
            .get_mut(&(tag, true))
            .unwrap()
            .parser
            .set_for_test(header(), t.clone());

        let data = SbrData {
            coupling: false,
            channels: vec![one_channel(n_q, n_low), one_channel(n_q, n_low)],
        };

        // A steady tone, not silence: silence round-trips trivially (every
        // sample is already continuous at 0), which would hide exactly the
        // discontinuity this test exists to catch.
        let core_au = || -> Vec<f32> { (0..1024).map(|i| (i as f32 * 0.1).sin() * 0.2).collect() };
        let mut planes = vec![core_au(), core_au()];
        chain.apply(tag, true, &data, &mut planes);

        let expected_len = 32 * SYNTHESIS_BANDS;
        for (ch, plane) in planes.iter().enumerate() {
            assert_eq!(
                plane.len(),
                expected_len,
                "ch{ch} AU1 didn't double to {expected_len} samples"
            );
        }

        // Second AU through the SAME chain/state, as consecutive AUs in one
        // stream are: this is what exercises the QMF history/HF-chirp/noise
        // generator continuity across the junction, not just each AU in
        // isolation.
        let mut planes2 = vec![core_au(), core_au()];
        chain.apply(tag, true, &data, &mut planes2);
        for (ch, plane) in planes2.iter().enumerate() {
            assert_eq!(
                plane.len(),
                expected_len,
                "ch{ch} AU2 didn't double to {expected_len} samples"
            );
        }

        // Cumulative length across both AUs is exactly 2x2048, and the
        // junction (last sample of AU1 next to the first of AU2) is not a
        // discontinuity spike: a dropped/duplicated-AU bug (the one fixed by
        // `apply_last`) either changes this total length or opens a silent
        // gap, and a QMF history corruption at the boundary spikes the
        // derivative right at the join far above the steady-state
        // sample-to-sample delta the continuous tone otherwise has.
        for ch in 0..2 {
            let mut joined = planes[ch].clone();
            joined.extend_from_slice(&planes2[ch]);
            assert_eq!(joined.len(), 2 * expected_len);

            let steady_delta = |s: &[f32]| -> f32 {
                s.windows(2)
                    .skip(expected_len / 2)
                    .take(expected_len / 4)
                    .map(|w| (w[1] - w[0]).abs())
                    .fold(0.0f32, f32::max)
            };
            let typical = steady_delta(&joined);
            let junction_delta = (joined[expected_len] - joined[expected_len - 1]).abs();
            assert!(
                junction_delta <= typical * 4.0 + 1e-6,
                "ch{ch} junction delta {junction_delta} far exceeds steady-state delta {typical} -- AU-boundary discontinuity"
            );
        }
    }

    /// §4.4: `element_instance_tag` is a namespace per element *type* --
    /// an encoder may legally give an SCE and a CPE in the same access
    /// unit the same tag value (this is exactly what a 5.1 stream with
    /// SCE+CPE+CPE+LFE does). `SbrChain` used to key its per-element state
    /// by the bare tag, so the CPE's `parse`/`apply` would silently reuse
    /// (and corrupt) the SCE's parser/channel state. Keying by `(tag,
    /// is_cpe)` must keep them fully independent: driving distinct tones
    /// through an SCE and a same-tagged CPE must not bleed into each
    /// other's output.
    #[test]
    fn an_sce_and_a_same_tagged_cpe_do_not_share_state() {
        let t = tables();
        let n_low = t.n_low;
        let n_q = t.n_q;
        let mut chain = SbrChain::new(44100);
        let tag = 0u8;

        for is_cpe in [false, true] {
            let n_ch = if is_cpe { 2 } else { 1 };
            chain.elements.insert(
                (tag, is_cpe),
                Element {
                    parser: SbrParser::new(44100),
                    channels: (0..n_ch)
                        .map(|i| ChannelState::new(n_q, 10 + i as u32))
                        .collect(),
                    last_data: None,
            last_header: None,
                },
            );
            chain
                .elements
                .get_mut(&(tag, is_cpe))
                .unwrap()
                .parser
                .set_for_test(header(), t.clone());
        }

        let sce_data = SbrData {
            coupling: false,
            channels: vec![one_channel(n_q, n_low)],
        };
        let cpe_data = SbrData {
            coupling: false,
            channels: vec![one_channel(n_q, n_low), one_channel(n_q, n_low)],
        };

        let sce_tone = || -> Vec<f32> { (0..1024).map(|i| (i as f32 * 0.05).sin() * 0.2).collect() };
        let cpe_tone = || -> Vec<f32> { (0..1024).map(|i| (i as f32 * 0.37).sin() * 0.2).collect() };

        let mut sce_planes = vec![sce_tone()];
        chain.apply(tag, false, &sce_data, &mut sce_planes);
        let mut cpe_planes = vec![cpe_tone(), cpe_tone()];
        chain.apply(tag, true, &cpe_data, &mut cpe_planes);

        // If state aliased, the CPE call above would have run through (and
        // mutated) the SCE's Analysis/Synthesis/chirp/noise state instead
        // of its own -- re-running the SCE with the same input must give
        // the SAME output as the first call, proving its state was never
        // touched by the CPE call in between.
        let mut sce_planes2 = vec![sce_tone()];
        chain.apply(tag, false, &sce_data, &mut sce_planes2);

        // A fresh, never-driven SCE-only chain gives the reference: one AU
        // through tag/is_cpe=(0,false), nothing else touching its state.
        let mut reference = SbrChain::new(44100);
        reference.elements.insert(
            (tag, false),
            Element {
                parser: SbrParser::new(44100),
                channels: vec![ChannelState::new(n_q, 10)],
                last_data: None,
            last_header: None,
            },
        );
        reference
            .elements
            .get_mut(&(tag, false))
            .unwrap()
            .parser
            .set_for_test(header(), t.clone());
        let mut sce_ref1 = vec![sce_tone()];
        reference.apply(tag, false, &sce_data, &mut sce_ref1);
        let mut sce_ref2 = vec![sce_tone()];
        reference.apply(tag, false, &sce_data, &mut sce_ref2);

        assert_eq!(
            sce_planes[0], sce_ref1[0],
            "SCE tag 0's first call already diverges from an isolated reference"
        );
        assert_eq!(
            sce_planes2[0], sce_ref2[0],
            "SCE tag 0's state was mutated by the same-tagged CPE's apply() call in between"
        );
    }

    /// Round-13 stitching verdict (queue item 1): the QMF filterbank pair
    /// underneath the low band (`0..kx`, `apply_data` copies
    /// `raw[slot][0..kx]` straight from `Analysis` into `Synthesis`, no
    /// envelope/HF touch) must give BIT-IDENTICAL output whether its
    /// `process_slot` calls arrive chunked AU-by-AU (32 slots per call,
    /// matching a real 1024-sample core frame) or as one continuous run --
    /// `Analysis`/`Synthesis`'s own history/overlap-add state carries
    /// between calls regardless of how the caller groups them, so the
    /// SAME sample sequence fed either way must produce the SAME output.
    /// (An earlier version of this test drove it through `SbrChain::apply`
    /// with one AU-sized `SbrData` grid reused for a 10-AU-long "continuous"
    /// call; that showed a large divergence, but the divergence was a test
    /// bug, not a real one: `sbr_env::adjust`'s envelope gain is grid-scoped
    /// to `ch.t_env`'s own 32-slot range, so a 320-slot call only gain-
    /// adjusted its first AU's worth and left the other nine raw --
    /// `apply_data` is inherently one-AU-per-call by design, so the
    /// meaningful comparison is at the QMF layer these AU-sized calls sit
    /// on top of, not by force-feeding it a multi-AU span it was never
    /// meant to take in one call.)
    #[test]
    fn ten_aus_worth_of_qmf_slots_chained_per_au_are_bit_identical_to_one_continuous_pass() {
        let mut rng = 0x02f6_e2b1_u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let all_samples: Vec<f32> = (0..10 * 1024).map(|_| next() as f32 * 0.3).collect();

        // (a) Per-AU: a fresh 32-slot chunk fed to the SAME persistent
        // Analysis/Synthesis pair per call, as `apply_data` drives it once
        // per access unit.
        let mut analysis_a = Analysis::new();
        let mut synthesis_a = Synthesis::new();
        let mut per_au_out: Vec<f32> = Vec::new();
        for au in all_samples.chunks(1024) {
            for slot in au.chunks_exact(ANALYSIS_BANDS) {
                let mut chunk = [0f32; ANALYSIS_BANDS];
                chunk.copy_from_slice(slot);
                let sub = analysis_a.process_slot(&chunk);
                let mut v = [Complex::ZERO; SYNTHESIS_BANDS];
                v[0..ANALYSIS_BANDS].copy_from_slice(&sub);
                per_au_out.extend(synthesis_a.process_slot(&v));
            }
        }

        // (b) One continuous pass over the same samples, same bank pair,
        // just not grouped into per-AU calls.
        let mut analysis_b = Analysis::new();
        let mut synthesis_b = Synthesis::new();
        let mut continuous_out: Vec<f32> = Vec::new();
        for slot in all_samples.chunks_exact(ANALYSIS_BANDS) {
            let mut chunk = [0f32; ANALYSIS_BANDS];
            chunk.copy_from_slice(slot);
            let sub = analysis_b.process_slot(&chunk);
            let mut v = [Complex::ZERO; SYNTHESIS_BANDS];
            v[0..ANALYSIS_BANDS].copy_from_slice(&sub);
            continuous_out.extend(synthesis_b.process_slot(&v));
        }

        assert_eq!(per_au_out.len(), continuous_out.len());
        let mut max_diff = 0.0f32;
        let mut diverging = 0usize;
        for (a, b) in per_au_out.iter().zip(&continuous_out) {
            let d = (a - b).abs();
            if d != 0.0 {
                diverging += 1;
            }
            max_diff = max_diff.max(d);
        }
        let total = per_au_out.len();
        println!("stitching check: {diverging}/{total} samples differ, max abs diff {max_diff:e}");
        assert!(
            max_diff < 1e-5,
            "per-AU chaining and one continuous pass diverge by {max_diff:e} -- a real QMF stitching bug"
        );
    }
}

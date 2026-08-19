//! Wires the SBR payload parser, HF generator and envelope adjuster into
//! one per-element chain the decoder's `raw_data_block` loop drives: parse
//! a FIL element's `sbr_extension_data`, run the previous SCE/CPE's core
//! PCM through 32-band analysis, generate and adjust the HF region, and
//! resynthesize through the 64-band bank to double-rate PCM in place.

use crate::sbr_env::{self, NoiseGen};
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
        "SBRDBG frame {n} ch{ch_idx}: amp_res={} kx={} k2={} n_low={} n_high={} n_q={} t_env={:?} freq_res={:?} t_noise={:?}",
        header.amp_res,
        tables.kx,
        tables.k2,
        tables.n_low,
        tables.n_high,
        tables.n_q,
        ch.t_env,
        ch.freq_res,
        ch.t_noise
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

/// Per-channel DSP state an SBR element's reconstruction carries across
/// frames: the QMF filterbank pair, the HF generator's LPC history and
/// chirp factors, and the noise-floor generator.
struct ChannelState {
    analysis: Analysis,
    synthesis: Synthesis,
    hf_hist: HfHistory,
    chirp: ChirpState,
    noise: NoiseGen,
}

impl ChannelState {
    fn new(n_q: usize, seed: u32) -> ChannelState {
        ChannelState {
            analysis: Analysis::new(),
            synthesis: Synthesis::new(),
            hf_hist: HfHistory::new(ANALYSIS_BANDS),
            chirp: ChirpState::new(n_q.max(1)),
            noise: NoiseGen::new(seed),
        }
    }
}

/// One SBR element's (SCE or CPE) persistent state.
struct Element {
    parser: SbrParser,
    channels: Vec<ChannelState>,
}

/// Every configured SBR element in a stream, keyed by
/// `element_instance_tag` (assumed stable frame to frame, as every
/// encoder in practice emits it).
pub struct SbrChain {
    rate: u32,
    elements: HashMap<u8, Element>,
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
        let elem = self.elements.entry(tag).or_insert_with(|| Element {
            parser: SbrParser::new(self.rate),
            channels: Vec::new(),
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
    pub fn apply(&mut self, tag: u8, data: &SbrData, planes: &mut [Vec<f32>]) {
        let Some(elem) = self.elements.get_mut(&tag) else {
            return;
        };
        let Some(tables) = elem.parser.tables().cloned() else {
            return;
        };
        let Some(header) = elem.parser.header().cloned() else {
            return;
        };
        let coupling = data.coupling;
        for (ch, plane) in planes.iter_mut().enumerate() {
            if ch >= data.channels.len() || ch >= elem.channels.len() {
                continue;
            }
            let num_slots = plane.len() / ANALYSIS_BANDS;
            if num_slots == 0 {
                continue;
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
            let mut hf = sbr_hf::generate(
                &low_cur,
                &tables,
                &sbr_ch.invf_mode,
                &mut state.chirp,
                &mut state.hf_hist,
            );
            let (env_energy, noise_energy) =
                sbr_env::dequantize_frame(&header, &data.channels, ch, coupling);
            // sbr_env::adjust works in QMF-slot units; the parsed borders
            // are in the coarser envelope-time-slot units (see RATE above).
            let scaled = SbrChannel {
                t_env: sbr_ch.t_env.iter().map(|&t| t * RATE).collect(),
                t_noise: sbr_ch.t_noise.iter().map(|&t| t * RATE).collect(),
                ..sbr_ch.clone()
            };
            sbr_env::adjust(
                &mut hf,
                &tables,
                &header,
                &scaled,
                &env_energy,
                &noise_energy,
                &mut state.noise,
            );

            let kx = (tables.kx as usize).min(ANALYSIS_BANDS);
            let k2 = (tables.k2 as usize).min(SYNTHESIS_BANDS);
            let mut out = Vec::with_capacity(num_slots * SYNTHESIS_BANDS);
            for slot in 0..num_slots {
                let mut v = [Complex::ZERO; SYNTHESIS_BANDS];
                v[0..kx].copy_from_slice(&raw[slot][0..kx]);
                for b in kx..k2 {
                    if b - kx < hf.len() {
                        v[b] = hf[b - kx][slot];
                    }
                }
                let mut pcm = state.synthesis.process_slot(&v);
                for s in &mut pcm {
                    *s *= crate::decode::OUTPUT_SCALE;
                }
                out.extend_from_slice(&pcm);
            }
            *plane = out;
        }
    }
}

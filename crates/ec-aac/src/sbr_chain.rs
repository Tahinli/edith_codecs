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
                let sub = state.analysis.process_slot(&chunk);
                for b in 0..ANALYSIS_BANDS {
                    low_cur[b][slot] = sub[b];
                }
                raw.push(sub);
            }

            let sbr_ch = &data.channels[ch];
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
                let pcm = state.synthesis.process_slot(&v);
                out.extend_from_slice(&pcm);
            }
            *plane = out;
        }
    }
}

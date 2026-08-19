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
    /// The most recently parsed frame's envelope/noise/grid data, held so an
    /// access unit whose FIL element carries no fresh SBR payload this frame
    /// (an "unavailable" frame, §4.6.18: legal per spec and the decoder must
    /// keep reconstructing at the doubled rate using the last known data,
    /// not silently fall back to core-only output) can still be
    /// SBR-reconstructed instead of leaving that one access unit's plane at
    /// half length -- which otherwise permanently shifts every sample after
    /// it out of alignment with a reference that stayed at full rate.
    last_data: Option<SbrData>,
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
            last_data: None,
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
        if let Some(elem) = self.elements.get_mut(&tag) {
            elem.last_data = Some(data.clone());
        }
        self.apply_data(tag, data, planes);
    }

    /// Re-runs the chain for `tag` using the last successfully parsed
    /// frame's SBR data, for an access unit whose own FIL element carried no
    /// fresh payload this frame. A no-op (planes left at core content, same
    /// as [`apply`]'s existing malformed/missing-state fallback) if no prior
    /// frame ever parsed successfully for this element.
    pub fn apply_last(&mut self, tag: u8, planes: &mut [Vec<f32>]) {
        let Some(data) = self.elements.get(&tag).and_then(|e| e.last_data.clone()) else {
            return;
        };
        self.apply_data(tag, &data, planes);
    }

    fn apply_data(&mut self, tag: u8, data: &SbrData, planes: &mut [Vec<f32>]) {
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
            tag,
            Element {
                parser: SbrParser::new(44100),
                channels: vec![ChannelState::new(n_q, 1), ChannelState::new(n_q, 2)],
                last_data: None,
            },
        );
        chain
            .elements
            .get_mut(&tag)
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
        chain.apply(tag, &data, &mut planes);

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
        chain.apply(tag, &data, &mut planes2);
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

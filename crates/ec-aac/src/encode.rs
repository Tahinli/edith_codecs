//! AAC-LC encoding: psychoacoustics, window decision, quantisation and the
//! `raw_data_block` writer.
//!
//! The psychoacoustic model is the classic three-stage one, and this is exactly
//! what it does: a 2048-point FFT of the windowed input gives per-band energy;
//! energy is spread across bands with a two-slope spreading function at
//! scalefactor-band resolution; tonality is estimated by each band's spectral
//! flatness (**not** by phase unpredictability, which needs two frames of
//! history for a fraction of a dB); the signal-to-mask ratio runs from 6 dB on
//! noise-like bands to 24 dB on tonal ones.  Rate control is the two-loop
//! search of the informative part of ISO/IEC 14496-3: an inner distortion loop
//! choosing each band's scalefactor against its mask, and an outer rate loop
//! over a common offset, against a bit reservoir.
//!
//! The window decision is shared by every channel of a frame, which is what
//! lets a channel pair use `common_window` and therefore mid/side coding.
//!
//! Band, group and window indices are the domain's own coordinates and address
//! several parallel arrays at once, so the loops here are written over indices
//! rather than iterators.
#![allow(clippy::needless_range_loop)]

use ec_core::{BitWriter, Error, Result};
use ec_dsp::{Complex, Mdct, RealFft, Window};

use crate::config::{AdtsHeader, config_for_channels, sf_index_for_rate};
use crate::decode::{FRAME_LEN, SHORT_LEN, WindowSequence};
use crate::tables::{CODEBOOKS, Codebook, SCALEFACTOR_CODES, SWB_LONG, SWB_SHORT};

const SF_OFFSET: i32 = 100;
/// The decoder divides by this; the encoder multiplies by it, so a round trip
/// is unity gain.
const INPUT_SCALE: f32 = 65536.0;
/// Quantiser rounding bias of the informative encoder (§4.6.2).
const ROUND_BIAS: f32 = 0.4054;
/// Ceiling on what the reservoir may lend one frame, per channel.
const RESERVOIR_MAX: i32 = 6144;
/// Largest magnitude the escape codebook can carry.
const MAX_QUANT: i32 = 8191;
/// Masking floor relative to the loudest band of the frame, standing in for the
/// absolute threshold of hearing (see [`AacEncoder::psychoacoustics`]).
const ATH_FLOOR: f32 = 1e-7;

/// Which window shape the encoder asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowShape {
    /// Sine window everywhere.
    #[default]
    Sine,
    /// Kaiser-Bessel-derived everywhere: better stop-band rejection, which
    /// suits tonal material.
    Kbd,
}

/// Encoder options.
#[derive(Debug, Clone, Copy)]
pub struct AacEncoderConfig {
    /// Target bitrate in bits per second, over all channels.
    pub bitrate_bps: u32,
    /// Window shape policy.
    pub window_shape: WindowShape,
    /// Emit ADTS framing around each packet instead of a bare `raw_data_block`.
    pub adts: bool,
    /// Allow mid/side coding on channel pairs.
    pub mid_side: bool,
    /// Allow the long/short window switch on transients.
    pub window_switching: bool,
}

impl Default for AacEncoderConfig {
    fn default() -> AacEncoderConfig {
        AacEncoderConfig {
            bitrate_bps: 128_000,
            window_shape: WindowShape::Sine,
            adts: false,
            mid_side: true,
            window_switching: true,
        }
    }
}

/// One encoded access unit.
#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub data: Vec<u8>,
    /// Presentation time in samples (frame index * 1024).
    pub pts: i64,
    /// Duration in samples; always one AAC frame.
    pub duration: u32,
}

/// One channel's transform and masking thresholds for a frame.
#[derive(Clone)]
struct ChannelFrame {
    /// MDCT coefficients, window-major for short sequences.
    coef: Vec<f32>,
    /// Masking threshold per band.
    threshold: Vec<f32>,
}

/// An AAC-LC encoder.
pub struct AacEncoder {
    config: AacEncoderConfig,
    sample_rate: u32,
    sf_index: u8,
    channels: usize,
    /// Input not yet turned into frames, per channel.
    pcm: Vec<Vec<f32>>,
    /// The previous frame's samples, the MDCT block's left half.
    prev: Vec<Vec<f32>>,
    seq: WindowSequence,
    energy: f32,
    mdct_long: Mdct<f32>,
    mdct_short: Mdct<f32>,
    fft: RealFft<f32>,
    sine_long: Vec<f32>,
    kbd_long: Vec<f32>,
    sine_short: Vec<f32>,
    kbd_short: Vec<f32>,
    queue: std::collections::VecDeque<EncodedPacket>,
    frames_out: i64,
    reservoir: i32,
    flushed: bool,
    initialised: bool,
}

impl AacEncoder {
    pub fn new(config: AacEncoderConfig) -> AacEncoder {
        AacEncoder {
            config,
            sample_rate: 0,
            sf_index: 3,
            channels: 0,
            pcm: Vec::new(),
            prev: Vec::new(),
            seq: WindowSequence::OnlyLong,
            energy: 0.0,
            mdct_long: Mdct::new(2 * FRAME_LEN),
            mdct_short: Mdct::new(2 * SHORT_LEN),
            fft: RealFft::new(2 * FRAME_LEN),
            sine_long: Window::<f32>::sine(2 * FRAME_LEN).as_slice().to_vec(),
            kbd_long: Window::<f32>::kbd(2 * FRAME_LEN, 4.0).as_slice().to_vec(),
            sine_short: Window::<f32>::sine(2 * SHORT_LEN).as_slice().to_vec(),
            kbd_short: Window::<f32>::kbd(2 * SHORT_LEN, 6.0).as_slice().to_vec(),
            queue: std::collections::VecDeque::new(),
            frames_out: 0,
            reservoir: 0,
            flushed: false,
            initialised: false,
        }
    }

    /// The stream's sample rate; 0 until the first PCM arrives.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// The stream's channel count; 0 until the first PCM arrives.
    pub fn channels(&self) -> u16 {
        self.channels as u16
    }

    /// The AudioSpecificConfig a container needs for this stream.
    pub fn audio_specific_config(&self) -> Vec<u8> {
        crate::config::audio_specific_config_bytes(self.sample_rate, self.channels as u16)
    }

    fn init(&mut self, channels: u16, sample_rate: u32) -> Result<()> {
        if self.initialised {
            if self.channels != usize::from(channels) || self.sample_rate != sample_rate {
                return Err(Error::corrupt("aac: stream parameters changed mid-encode"));
            }
            return Ok(());
        }
        let sf_index = sf_index_for_rate(sample_rate).ok_or_else(|| {
            Error::unsupported("aac", format!("{sample_rate} Hz is not an AAC sample rate"))
        })?;
        if channels == 0 || channels == 7 || channels > 8 {
            return Err(Error::unsupported(
                "aac",
                format!("{channels} channels has no channelConfiguration"),
            ));
        }
        self.sf_index = sf_index;
        self.sample_rate = sample_rate;
        self.channels = usize::from(channels);
        self.pcm = vec![Vec::new(); self.channels];
        self.prev = vec![vec![0.0; FRAME_LEN]; self.channels];
        self.initialised = true;
        Ok(())
    }

    /// Appends interleaved PCM.
    pub fn push_pcm(&mut self, interleaved: &[f32], channels: u16, sample_rate: u32) -> Result<()> {
        self.init(channels, sample_rate)?;
        let n = usize::from(channels);
        for (i, sample) in interleaved.iter().enumerate() {
            self.pcm[i % n].push(*sample);
        }
        self.drain_frames();
        Ok(())
    }

    /// Appends planar PCM, one slice per channel.
    pub fn push_pcm_planar(&mut self, planes: &[&[f32]], sample_rate: u32) -> Result<()> {
        self.init(planes.len() as u16, sample_rate)?;
        for (buf, plane) in self.pcm.iter_mut().zip(planes) {
            buf.extend_from_slice(plane);
        }
        self.drain_frames();
        Ok(())
    }

    /// Marks the end of input; the tail is padded to a whole frame.
    pub fn finish(&mut self) {
        if !self.initialised || self.flushed {
            self.flushed = true;
            return;
        }
        self.flushed = true;
        let left = self.pcm.iter().map(|c| c.len()).max().unwrap_or(0);
        if left > 0 {
            // One frame of lookahead is held back for the window decision, and
            // the filterbank needs another frame of zeroes to flush its tail.
            let pad = left.div_ceil(FRAME_LEN) * FRAME_LEN + 2 * FRAME_LEN;
            for c in self.pcm.iter_mut() {
                c.resize(pad, 0.0);
            }
        }
        self.drain_frames();
    }

    /// Takes the next encoded packet, or [`Error::Eof`] once drained.
    pub fn next_packet(&mut self) -> Result<EncodedPacket> {
        self.queue.pop_front().ok_or(Error::Eof)
    }

    fn drain_frames(&mut self) {
        while self.pcm.iter().all(|c| c.len() >= 2 * FRAME_LEN) {
            self.encode_frame();
        }
    }

    fn long_window(&self) -> &[f32] {
        match self.config.window_shape {
            WindowShape::Sine => &self.sine_long,
            WindowShape::Kbd => &self.kbd_long,
        }
    }

    fn short_window(&self) -> &[f32] {
        match self.config.window_shape {
            WindowShape::Sine => &self.sine_short,
            WindowShape::Kbd => &self.kbd_short,
        }
    }

    fn swb(&self, seq: WindowSequence) -> &'static [u16] {
        let idx = usize::from(self.sf_index).min(11);
        if seq == WindowSequence::EightShort {
            SWB_SHORT[idx]
        } else {
            SWB_LONG[idx]
        }
    }

    /// Transform, psychoacoustics, quantisation and bitstream for one frame.
    fn encode_frame(&mut self) {
        // One window decision for the whole frame: a channel pair can only
        // share `ics_info` -- and so only use mid/side -- if it shares its
        // window sequence.
        let transient = self.config.window_switching && self.frame_transient();
        self.seq = match self.seq {
            WindowSequence::LongStart => WindowSequence::EightShort,
            WindowSequence::EightShort if transient => WindowSequence::EightShort,
            WindowSequence::EightShort => WindowSequence::LongStop,
            _ if transient => WindowSequence::LongStart,
            _ => WindowSequence::OnlyLong,
        };
        let seq = self.seq;

        let mut frames = Vec::with_capacity(self.channels);
        for ch in 0..self.channels {
            let cur: Vec<f32> = self.pcm[ch][..FRAME_LEN]
                .iter()
                .map(|v| v * INPUT_SCALE)
                .collect();
            let mut block = Vec::with_capacity(2 * FRAME_LEN);
            block.extend_from_slice(&self.prev[ch]);
            block.extend_from_slice(&cur);
            self.prev[ch] = cur;
            let coef = self.transform(&block, seq);
            let threshold = self.psychoacoustics(&block, &coef, seq);
            frames.push(ChannelFrame { coef, threshold });
        }
        for c in self.pcm.iter_mut() {
            c.drain(..FRAME_LEN);
        }

        let target = (u64::from(self.config.bitrate_bps) * FRAME_LEN as u64
            / u64::from(self.sample_rate.max(1))) as i32;
        let budget = target
            + self
                .reservoir
                .clamp(0, RESERVOIR_MAX * self.channels as i32);
        let data = self.write_frame(&frames, seq, budget);
        let used = data.len() as i32 * 8;
        self.reservoir =
            (self.reservoir + target - used).clamp(0, RESERVOIR_MAX * self.channels as i32);
        let data = if self.config.adts {
            let header = AdtsHeader {
                object_type: 2,
                sf_index: self.sf_index,
                sample_rate: self.sample_rate,
                channels: self.channels as u16,
                channel_config: config_for_channels(self.channels as u16),
                frame_length: data.len() + 7,
                header_len: 7,
                raw_blocks: 1,
            };
            let mut out = crate::config::write_adts_header(&header);
            out.extend_from_slice(&data);
            out
        } else {
            data
        };
        self.queue.push_back(EncodedPacket {
            data,
            pts: self.frames_out * FRAME_LEN as i64,
            duration: FRAME_LEN as u32,
        });
        self.frames_out += 1;
    }

    /// An eighth-frame energy jump anywhere in the frame ahead is a transient.
    ///
    /// The running average is primed by the first frame rather than starting at
    /// zero: from zero every opening sample looks like an attack, and the frames
    /// that carry a file's first second would all be coded short.
    fn frame_transient(&mut self) -> bool {
        let mut hit = false;
        let primed = self.energy > 0.0;
        let mut running = self.energy;
        for i in 0..8 {
            let mut e = 0.0f32;
            for ch in 0..self.channels {
                let lo = FRAME_LEN + i * SHORT_LEN;
                for v in &self.pcm[ch][lo..lo + SHORT_LEN] {
                    e += v * v;
                }
            }
            e /= (SHORT_LEN * self.channels) as f32;
            if primed && e > running * 8.0 && e > 1e-7 {
                hit = true;
            }
            running = if i == 0 && !primed {
                e
            } else {
                running * 0.7 + e * 0.3
            };
        }
        self.energy = running;
        hit
    }

    fn transform(&mut self, block: &[f32], seq: WindowSequence) -> Vec<f32> {
        let mut coef = vec![0.0f32; FRAME_LEN];
        if seq == WindowSequence::EightShort {
            let win = self.short_window().to_vec();
            for w in 0..8 {
                let base = 448 + w * SHORT_LEN;
                self.mdct_short.forward_windowed(
                    &block[base..base + 2 * SHORT_LEN],
                    &win,
                    &mut coef[w * SHORT_LEN..(w + 1) * SHORT_LEN],
                );
            }
            return coef;
        }
        let long = self.long_window().to_vec();
        let short = self.short_window().to_vec();
        let mut win = vec![1.0f32; 2 * FRAME_LEN];
        match seq {
            WindowSequence::LongStart => {
                win[..FRAME_LEN].copy_from_slice(&long[..FRAME_LEN]);
                win[FRAME_LEN + 448..FRAME_LEN + 576]
                    .copy_from_slice(&short[SHORT_LEN..2 * SHORT_LEN]);
                win[FRAME_LEN + 576..].fill(0.0);
            }
            WindowSequence::LongStop => {
                win[..448].fill(0.0);
                win[448..576].copy_from_slice(&short[..SHORT_LEN]);
                win[FRAME_LEN..].copy_from_slice(&long[FRAME_LEN..]);
            }
            _ => win.copy_from_slice(&long),
        }
        self.mdct_long.forward_windowed(block, &win, &mut coef);
        coef
    }

    /// Per-band masking thresholds; see the module docs for the model.
    fn psychoacoustics(&mut self, block: &[f32], coef: &[f32], seq: WindowSequence) -> Vec<f32> {
        let swb = self.swb(seq);
        let bands = swb.len() - 1;
        let windowed: Vec<f32> = block
            .iter()
            .zip(self.sine_long.iter())
            .map(|(v, w)| v * w)
            .collect();
        let mut spectrum = vec![
            Complex {
                re: 0.0f32,
                im: 0.0f32
            };
            FRAME_LEN + 1
        ];
        self.fft.forward(&windowed, &mut spectrum);
        let short = seq == WindowSequence::EightShort;
        let windows = if short { 8 } else { 1 };
        let mut energy = vec![0.0f32; bands];
        let mut flatness = vec![0.0f32; bands];
        for b in 0..bands {
            let (lo, hi) = (usize::from(swb[b]), usize::from(swb[b + 1]));
            // Level comes from the MDCT itself, so the threshold and the
            // quantisation noise it is compared against are in the same units;
            // an FFT-scaled threshold would be a constant factor out, and that
            // constant is the difference between masking a band and coding it.
            let mut sum = 0.0f64;
            let mut n = 0usize;
            for w in 0..windows {
                let base = if short { w * SHORT_LEN } else { 0 };
                for k in base + lo..base + hi {
                    sum += f64::from(coef[k]) * f64::from(coef[k]);
                    n += 1;
                }
            }
            energy[b] = (sum / n.max(1) as f64) as f32;
            // Tonality comes from the FFT, which is what it is good for. A short
            // block's bands index 128 lines against the FFT's 1024, the same
            // eightfold ratio the filterbank uses.
            let (flo, fhi) = if short { (lo * 8, hi * 8) } else { (lo, hi) };
            let fhi = fhi.min(FRAME_LEN);
            let fn_ = fhi.saturating_sub(flo).max(1);
            let mut fsum = 0.0f64;
            let mut log_sum = 0.0f64;
            for k in flo..fhi {
                let p = f64::from(spectrum[k].norm_sqr()) + 1e-9;
                fsum += p;
                log_sum += p.ln();
            }
            let geometric = (log_sum / fn_ as f64).exp();
            flatness[b] = (geometric / (fsum / fn_ as f64).max(1e-30)) as f32;
        }
        // Two-slope spreading at band resolution: about 10 dB per band upward
        // in frequency, 25 dB downward.
        let mut spread = energy.clone();
        for b in 1..bands {
            spread[b] = spread[b].max(spread[b - 1] * 0.1);
        }
        for b in (0..bands.saturating_sub(1)).rev() {
            spread[b] = spread[b].max(spread[b + 1] * 0.003);
        }
        // Standing in for the absolute threshold of hearing: nothing this far
        // under the loudest band of the frame is audible, and without a floor
        // of this kind the spreading function falls away faster than the
        // filterbank's own leakage does, so an encoder spends its whole budget
        // coding the skirts of a tone at full precision.
        let floor = energy.iter().copied().fold(0.0f32, f32::max) * ATH_FLOOR;
        (0..bands)
            .map(|b| {
                // Flat (noise-like) bands mask well, peaky (tonal) ones badly.
                let tonality = (1.0 - flatness[b]).clamp(0.0, 1.0);
                let smr = 10f32.powf((6.0 + 18.0 * tonality) / 10.0);
                (spread[b] / smr).max(floor).max(1e-3)
            })
            .collect()
    }

    /// Quantises every channel against a shared rate loop and writes the block.
    fn write_frame(
        &mut self,
        frames: &[ChannelFrame],
        seq: WindowSequence,
        budget: i32,
    ) -> Vec<u8> {
        let layout = element_layout(self.channels);
        let swb = self.swb(seq);
        let short = seq == WindowSequence::EightShort;
        let max_sfb = (swb.len() - 1).min(if short { 15 } else { 51 });

        // Mid/side per band, decided once: the pair that codes to the more
        // lopsided energy split is the one that costs fewer bits.
        let mut elements: Vec<ElementPlan> = Vec::with_capacity(layout.len());
        for (element, chans) in &layout {
            let mut pair: Vec<ChannelFrame> = chans.iter().map(|&c| frames[c].clone()).collect();
            let mut ms = vec![false; max_sfb];
            if *element == Element::Cpe && self.config.mid_side {
                for b in 0..max_sfb {
                    if ms_wins(&pair[0], &pair[1], swb, b, short) {
                        ms[b] = true;
                    }
                }
                apply_ms(&mut pair, &ms, swb, max_sfb, short);
            }
            elements.push(ElementPlan {
                element: *element,
                frames: pair,
                ms,
            });
        }

        // Outer rate loop. Bits fall monotonically as the common offset rises,
        // so the finest quantisation that fits the budget is a bisection --
        // which also means the psychoacoustic model only has to get the shape
        // of the allocation right, not its absolute level.
        let cost = |enc: &AacEncoder, offset: i32| -> (i32, Vec<Coded>) {
            let mut coded = Vec::new();
            let mut bits = 10i32; // END element plus byte-alignment slack
            for plan in &elements {
                bits += 3
                    + 4
                    + if plan.element == Element::Cpe {
                        1 + 2 + max_sfb as i32
                    } else {
                        0
                    };
                for frame in &plan.frames {
                    let c = enc.quantise(frame, seq, swb, max_sfb, offset);
                    bits += c.bits;
                    coded.push(c);
                }
            }
            (bits, coded)
        };
        let (mut lo, mut hi) = (-240i32, 240i32);
        let (mut bits, mut coded) = cost(self, hi);
        if bits > budget {
            // Even the coarsest setting overflows: take it and let the
            // reservoir absorb the overshoot.
            lo = hi;
        } else {
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let (b, c) = cost(self, mid);
                if b <= budget {
                    hi = mid;
                    bits = b;
                    coded = c;
                } else {
                    lo = mid + 1;
                }
            }
            if lo < 240 {
                let (b, c) = cost(self, lo);
                if b <= budget {
                    bits = b;
                    coded = c;
                }
            }
        }
        let _ = (lo, bits);

        let mut w = BitWriter::new();
        let mut at = 0usize;
        for plan in &elements {
            let id = match plan.element {
                Element::Sce => 0u32,
                Element::Cpe => 1,
                Element::Lfe => 3,
            };
            w.write_bits(id, 3);
            w.write_bits(0, 4); // element_instance_tag
            if plan.element == Element::Cpe {
                w.write_bit(true); // common_window
                write_ics_info(&mut w, &coded[at]);
                w.write_bits(1, 2); // ms_mask_present: per band
                for b in 0..coded[at].max_sfb {
                    w.write_bit(plan.ms[b]);
                }
                write_ics(&mut w, &coded[at], false);
                write_ics(&mut w, &coded[at + 1], false);
                at += 2;
            } else {
                write_ics(&mut w, &coded[at], true);
                at += 1;
            }
        }
        w.write_bits(7, 3); // END
        w.align_to_byte();
        w.into_bytes()
    }

    /// Inner distortion loop: the coarsest scalefactor per band whose noise
    /// still sits under the mask, then codebook choice and an exact bit count.
    ///
    /// Only bands that actually code anything carry a scalefactor, and the
    /// codebook can only code a delta of +/-60 between consecutive ones. Both
    /// facts are settled here rather than at write time -- a scalefactor the
    /// bitstream cannot carry would come back at the wrong gain, and chaining
    /// the constraint through *silent* bands is what drags a fine scalefactor
    /// into the empty top of the spectrum and spends the frame on leakage.
    fn quantise(
        &self,
        frame: &ChannelFrame,
        seq: WindowSequence,
        swb: &'static [u16],
        max_sfb: usize,
        offset: i32,
    ) -> Coded {
        let short = seq == WindowSequence::EightShort;
        let windows = if short { 8 } else { 1 };
        let mut sf = vec![0i32; max_sfb];
        let mut floor = vec![0i32; max_sfb];
        let mut zero = vec![true; max_sfb];
        for b in 0..max_sfb {
            let (lo, hi) = (usize::from(swb[b]), usize::from(swb[b + 1]));
            let mut peak = 0.0f32;
            for w in 0..windows {
                let base = if short { w * SHORT_LEN } else { 0 };
                for k in base + lo..base + hi {
                    peak = peak.max(frame.coef[k].abs());
                }
            }
            if peak < 1e-4 {
                continue;
            }
            // The rate loop scales the whole mask rather than shifting the
            // scalefactors: that keeps the noise following the shape the model
            // asked for, and lets a band whose signal falls under its own mask
            // drop out entirely instead of being coded ever more coarsely.
            let target = frame.threshold[b] * 2f32.powf(offset as f32 * 0.5);
            let mut energy = 0.0f32;
            let mut count = 0usize;
            for w in 0..windows {
                let base = if short { w * SHORT_LEN } else { 0 };
                for k in base + lo..base + hi {
                    energy += frame.coef[k] * frame.coef[k];
                    count += 1;
                }
            }
            if energy / count.max(1) as f32 <= target {
                continue;
            }
            floor[b] = sf_floor(peak).max(0);
            let mut best = floor[b];
            let mut level = best;
            while level < 255 {
                if self.band_noise(frame, swb, b, level, windows, short) > target {
                    break;
                }
                best = level;
                level += 1;
            }
            sf[b] = best.clamp(floor[b], 255);
            zero[b] = false;
        }

        let mut quant = vec![0i32; FRAME_LEN];
        let mut books = vec![0u8; max_sfb];
        for _ in 0..4 {
            let mut settled = true;
            let mut prev: Option<i32> = None;
            for b in 0..max_sfb {
                if zero[b] {
                    continue;
                }
                let fixed = match prev {
                    None => sf[b],
                    Some(p) => sf[b].clamp(p - 60, p + 60).clamp(0, 255),
                };
                if fixed != sf[b] {
                    sf[b] = fixed;
                    settled = false;
                }
                prev = Some(sf[b]);
            }
            for b in 0..max_sfb {
                let (lo, hi) = (usize::from(swb[b]), usize::from(swb[b + 1]));
                if zero[b] {
                    books[b] = 0;
                    for w in 0..windows {
                        let base = if short { w * SHORT_LEN } else { 0 };
                        quant[base + lo..base + hi].fill(0);
                    }
                    continue;
                }
                let mut peak = 0i32;
                for w in 0..windows {
                    let base = if short { w * SHORT_LEN } else { 0 };
                    for k in lo..hi {
                        let q = quantise_one(frame.coef[base + k], sf[b]);
                        quant[base + k] = q;
                        peak = peak.max(q.abs());
                    }
                }
                books[b] = codebook_for(peak);
                if peak == 0 {
                    // Nothing survived: the band leaves the delta chain, which
                    // may free the bands after it to go coarser.
                    zero[b] = true;
                    settled = false;
                }
            }
            if settled {
                break;
            }
        }

        let mut bits = 8 + 3; // global_gain plus the three tool flags
        let mut prev: Option<i32> = None;
        for b in 0..max_sfb {
            if books[b] == 0 {
                continue;
            }
            bits += band_bits(&quant, swb, b, books[b], windows, short);
            let value = prev.map_or(60, |p| (60 + sf[b] - p).clamp(0, 120));
            bits += i32::from(SCALEFACTOR_CODES[value as usize].0);
            prev = Some(sf[b]);
        }
        let mut sections = 0;
        let mut b = 0usize;
        while b < max_sfb {
            let book = books[b];
            while b < max_sfb && books[b] == book {
                b += 1;
            }
            sections += 1;
        }
        bits += sections * (4 + if short { 3 } else { 5 });
        Coded {
            seq,
            max_sfb,
            swb,
            sf,
            books,
            quant,
            bits,
            short,
        }
    }

    /// Quantisation noise energy of one band at a candidate scalefactor.
    fn band_noise(
        &self,
        frame: &ChannelFrame,
        swb: &[u16],
        band: usize,
        sf: i32,
        windows: usize,
        short: bool,
    ) -> f32 {
        let (lo, hi) = (usize::from(swb[band]), usize::from(swb[band + 1]));
        let gain = 2f32.powf((sf - SF_OFFSET) as f32 * 0.25);
        let mut noise = 0.0f32;
        let mut count = 0usize;
        for w in 0..windows {
            let base = if short { w * SHORT_LEN } else { 0 };
            for k in base + lo..base + hi {
                let x = frame.coef[k];
                let q = quantise_one(x, sf);
                let mut back = (q.unsigned_abs() as f32).powf(4.0 / 3.0) * gain;
                if q < 0 {
                    back = -back;
                }
                noise += (x - back) * (x - back);
                count += 1;
            }
        }
        noise / count.max(1) as f32
    }
}

struct ElementPlan {
    element: Element,
    frames: Vec<ChannelFrame>,
    ms: Vec<bool>,
}

/// A channel's quantised frame, ready to write.
struct Coded {
    seq: WindowSequence,
    max_sfb: usize,
    swb: &'static [u16],
    sf: Vec<i32>,
    books: Vec<u8>,
    quant: Vec<i32>,
    bits: i32,
    short: bool,
}

/// True when mid/side splits this band's energy more lopsidedly than left/right
/// does, which is when it costs fewer bits.
fn ms_wins(l: &ChannelFrame, r: &ChannelFrame, swb: &[u16], band: usize, short: bool) -> bool {
    let (lo, hi) = (usize::from(swb[band]), usize::from(swb[band + 1]));
    let windows = if short { 8 } else { 1 };
    let (mut el, mut er, mut em, mut es) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for w in 0..windows {
        let base = if short { w * SHORT_LEN } else { 0 };
        for k in base + lo..base + hi {
            let (a, b) = (f64::from(l.coef[k]), f64::from(r.coef[k]));
            el += a * a;
            er += b * b;
            em += (a + b) * (a + b) * 0.25;
            es += (a - b) * (a - b) * 0.25;
        }
    }
    let lr = (el.max(er) + 1e-9) / (el.min(er) + 1e-9);
    let ms = (em.max(es) + 1e-9) / (em.min(es) + 1e-9);
    ms > lr * 1.5
}

fn apply_ms(pair: &mut [ChannelFrame], ms: &[bool], swb: &[u16], max_sfb: usize, short: bool) {
    let windows = if short { 8 } else { 1 };
    for b in 0..max_sfb {
        if !ms[b] {
            continue;
        }
        let (lo, hi) = (usize::from(swb[b]), usize::from(swb[b + 1]));
        for w in 0..windows {
            let base = if short { w * SHORT_LEN } else { 0 };
            for k in base + lo..base + hi {
                let (a, b2) = (pair[0].coef[k], pair[1].coef[k]);
                pair[0].coef[k] = (a + b2) * 0.5;
                pair[1].coef[k] = (a - b2) * 0.5;
            }
        }
        // The side channel inherits the pair's tighter mask.
        let t = pair[0].threshold[b].min(pair[1].threshold[b]);
        pair[0].threshold[b] = t;
        pair[1].threshold[b] = t;
    }
}

/// The finest scalefactor at which a peak of this size still fits the escape
/// codebook: below it the quantiser saturates, which costs bits *and* adds
/// distortion.
fn sf_floor(peak: f32) -> i32 {
    let want = (0.75 * peak.log2() - (MAX_QUANT as f32).log2()) / 0.1875;
    SF_OFFSET + want.ceil() as i32
}

fn quantise_one(x: f32, sf: i32) -> i32 {
    if x == 0.0 {
        return 0;
    }
    let gain = 2f32.powf(-(sf - SF_OFFSET) as f32 * 0.25);
    let q = ((x.abs() * gain).powf(0.75) + ROUND_BIAS) as i32;
    let q = q.min(MAX_QUANT);
    if x < 0.0 { -q } else { q }
}

/// The cheapest codebook that can carry a peak magnitude.
fn codebook_for(peak: i32) -> u8 {
    match peak {
        0 => 0,
        1 => 1,
        2 => 3,
        3..=4 => 5,
        5..=7 => 7,
        8..=12 => 9,
        _ => 11,
    }
}

fn band_bits(
    quant: &[i32],
    swb: &[u16],
    band: usize,
    book: u8,
    windows: usize,
    short: bool,
) -> i32 {
    let cb = &CODEBOOKS[usize::from(book) - 1];
    let (lo, hi) = (usize::from(swb[band]), usize::from(swb[band + 1]));
    let dim = usize::from(cb.dim);
    let mut bits = 0i32;
    for w in 0..windows {
        let base = if short { w * SHORT_LEN } else { 0 };
        let mut k = lo;
        while k < hi {
            let end = (k + dim).min(hi);
            bits += tuple_bits(cb, &quant[base + k..base + end]);
            k += dim;
        }
    }
    bits
}

/// Bits one Huffman tuple costs, sign and escape included.
fn tuple_bits(cb: &Codebook, values: &[i32]) -> i32 {
    let mut bits = i32::from(cb.codes[tuple_index(cb, values)].0);
    for &v in values {
        if cb.unsigned && v != 0 {
            bits += 1;
        }
        if cb.esc && v.unsigned_abs() >= 16 {
            let n = escape_order(v.unsigned_abs());
            bits += 2 * n + 5;
        }
    }
    bits
}

/// The escape's unary length: `value` needs `n + 4` payload bits.
fn escape_order(mag: u32) -> i32 {
    (32 - mag.leading_zeros()) as i32 - 5
}

/// Tuple to codebook index, clamped into the codebook's range.
fn tuple_index(cb: &Codebook, values: &[i32]) -> usize {
    let span = if cb.unsigned {
        usize::from(cb.lav) + 1
    } else {
        2 * usize::from(cb.lav) + 1
    };
    let mut idx = 0usize;
    for i in 0..usize::from(cb.dim) {
        let v = values.get(i).copied().unwrap_or(0);
        let digit = if cb.unsigned {
            v.unsigned_abs().min(u32::from(cb.lav)) as usize
        } else {
            (v.clamp(-i32::from(cb.lav), i32::from(cb.lav)) + i32::from(cb.lav)) as usize
        };
        idx = idx * span + digit;
    }
    idx
}

fn write_tuple(w: &mut BitWriter, cb: &Codebook, values: &[i32]) {
    let (len, code) = cb.codes[tuple_index(cb, values)];
    w.write_bits(code, u32::from(len));
    if cb.unsigned {
        for &v in values.iter().take(usize::from(cb.dim)) {
            if v != 0 {
                w.write_bit(v < 0);
            }
        }
    }
    if cb.esc {
        for &v in values.iter().take(usize::from(cb.dim)) {
            let mag = v.unsigned_abs();
            if mag >= 16 {
                let n = escape_order(mag) as u32;
                for _ in 0..n {
                    w.write_bit(true);
                }
                w.write_bit(false);
                w.write_bits(mag & ((1 << (n + 4)) - 1), n + 4);
            }
        }
    }
}

fn write_ics_info(w: &mut BitWriter, c: &Coded) {
    w.write_bit(false); // ics_reserved_bit
    let seq = match c.seq {
        WindowSequence::OnlyLong => 0,
        WindowSequence::LongStart => 1,
        WindowSequence::EightShort => 2,
        WindowSequence::LongStop => 3,
    };
    w.write_bits(seq, 2);
    w.write_bit(false); // sine window shape
    if c.short {
        w.write_bits(c.max_sfb as u32, 4);
        w.write_bits(0x7F, 7); // one group of eight windows
    } else {
        w.write_bits(c.max_sfb as u32, 6);
        w.write_bit(false); // predictor_data_present
    }
}

fn write_ics(w: &mut BitWriter, c: &Coded, own_info: bool) {
    let first = c.books.iter().position(|&b| b != 0);
    let global_gain = first.map_or(100, |b| c.sf[b]).clamp(0, 255) as u32;
    w.write_bits(global_gain, 8);
    if own_info {
        write_ics_info(w, c);
    }
    // section_data
    let esc_bits = if c.short { 3 } else { 5 };
    let esc = (1u32 << esc_bits) - 1;
    let mut b = 0usize;
    while b < c.max_sfb {
        let book = c.books[b];
        let start = b;
        while b < c.max_sfb && c.books[b] == book {
            b += 1;
        }
        w.write_bits(u32::from(book), 4);
        let mut left = (b - start) as u32;
        while left >= esc {
            w.write_bits(esc, esc_bits);
            left -= esc;
        }
        w.write_bits(left, esc_bits);
    }
    // scale_factor_data
    let mut prev = global_gain as i32;
    for b in 0..c.max_sfb {
        if c.books[b] == 0 {
            continue;
        }
        let value = (60 + c.sf[b] - prev).clamp(0, 120) as usize;
        let (len, code) = SCALEFACTOR_CODES[value];
        w.write_bits(code, u32::from(len));
        prev += value as i32 - 60;
    }
    w.write_bit(false); // pulse_data_present
    w.write_bit(false); // tns_data_present
    w.write_bit(false); // gain_control_data_present
    // spectral_data
    let windows = if c.short { 8 } else { 1 };
    for b in 0..c.max_sfb {
        let book = c.books[b];
        if book == 0 {
            continue;
        }
        let cb = &CODEBOOKS[usize::from(book) - 1];
        let dim = usize::from(cb.dim);
        let (lo, hi) = (usize::from(c.swb[b]), usize::from(c.swb[b + 1]));
        for win in 0..windows {
            let base = if c.short { win * SHORT_LEN } else { 0 };
            let mut k = lo;
            while k < hi {
                let end = (k + dim).min(hi);
                write_tuple(w, cb, &c.quant[base + k..base + end]);
                k += dim;
            }
        }
    }
}

/// Which channel element carries which of the caller's channels. The caller
/// speaks film order (`FL, FR, FC, LFE, BL, BR, SL, SR`); the bitstream wants
/// centre first and LFE last (ISO/IEC 14496-3 tbl 1.19).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Element {
    Sce,
    Cpe,
    Lfe,
}

fn element_layout(channels: usize) -> Vec<(Element, Vec<usize>)> {
    match channels {
        1 => vec![(Element::Sce, vec![0])],
        2 => vec![(Element::Cpe, vec![0, 1])],
        3 => vec![(Element::Sce, vec![2]), (Element::Cpe, vec![0, 1])],
        4 => vec![
            (Element::Sce, vec![2]),
            (Element::Cpe, vec![0, 1]),
            (Element::Sce, vec![3]),
        ],
        5 => vec![
            (Element::Sce, vec![2]),
            (Element::Cpe, vec![0, 1]),
            (Element::Cpe, vec![3, 4]),
        ],
        6 => vec![
            (Element::Sce, vec![2]),
            (Element::Cpe, vec![0, 1]),
            (Element::Cpe, vec![4, 5]),
            (Element::Lfe, vec![3]),
        ],
        8 => vec![
            (Element::Sce, vec![2]),
            (Element::Cpe, vec![0, 1]),
            (Element::Cpe, vec![6, 7]),
            (Element::Cpe, vec![4, 5]),
            (Element::Lfe, vec![3]),
        ],
        n => (0..n).map(|i| (Element::Sce, vec![i])).collect(),
    }
}

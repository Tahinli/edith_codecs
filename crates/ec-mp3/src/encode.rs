//! Layer III encoding: analysis filterbank, MDCT with block switching, a
//! masking model, the two quantisation loops and the bit reservoir.
//!
//! **The psychoacoustic model, stated plainly.** Per granule and channel it
//! takes one 1024-point real FFT of the sine-windowed input, sums the power
//! spectrum into the granule's scalefactor bands, spreads that energy across
//! neighbouring bands with a one-sided exponential (about 25 dB/band up, 10
//! dB/band down), and sets each band's allowed noise to the spread energy
//! minus a signal-to-mask offset that follows a spectral-flatness tonality
//! estimate — 21 dB where the band looks tonal, 6 dB where it looks like
//! noise — floored by an absolute-threshold curve. It has no temporal
//! spreading, no pre-echo control beyond block switching, and no inter-channel
//! masking; those are the honest gaps against a full ISO Model 2.
//!
//! The outer (distortion) loop amplifies the scalefactors of bands whose
//! quantisation noise exceeds that threshold; the inner (rate) loop raises
//! `global_gain` until the Huffman-coded granule fits the bits the reservoir
//! makes available.

use crate::filterbank::{Analysis, alias_expand};
use crate::header::{ChannelMode, FrameHeader, Version};
use crate::huffman::{self, Table};
use crate::tables::{MAX_QUANT, SLEN, long_starts, power43, short_starts, windows};
use ec_core::bitio::BitWriter;
use ec_core::error::{Error, Result};
use ec_dsp::{RealFft, Window};

/// Encoder settings.
///
/// The field names match the incumbent `rusty_mp3` shim's, because the replica
/// swaps one for the other without touching call sites.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mp3EncoderConfig {
    /// Target bitrate in kbit/s, snapped to the nearest legal Layer III rate.
    pub bitrate_kbps: u32,
    /// Quality on a 0..1 scale instead of a bitrate.
    ///
    /// corner-cut: this picks a constant bitrate from the quality rather than
    /// varying the rate per frame — the streams are legal and decode
    /// everywhere, they just do not spend fewer bits on easy frames. Upgrade
    /// path is a per-frame bitrate index chosen after the rate loop, which the
    /// frame writer here already supports (it writes the header last).
    pub vbr_quality: Option<f32>,
}

const BITRATES_V1: [u32; 14] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];
const BITRATES_V2: [u32; 14] = [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// The legal Layer III bitrate nearest to `kbps` for this stream.
pub fn snap_bitrate(version: Version, kbps: u32) -> u32 {
    let table: &[u32] = if version == Version::Mpeg1 {
        &BITRATES_V1
    } else {
        &BITRATES_V2
    };
    *table
        .iter()
        .min_by_key(|&&r| r.abs_diff(kbps))
        .expect("bitrate table is not empty")
}

/// The bitrate a `vbr_quality` in `[0, 1]` asks for, in kbit/s.
pub fn bitrate_for_quality(quality: f32) -> u32 {
    let q = quality.clamp(0.0, 1.0);
    // 0.0 -> 96 kbit/s, 0.5 -> 192, 1.0 -> 320.
    (96.0 + q * 224.0).round() as u32
}

const SFB_LONG: usize = 21;

/// One granule's worth of coded spectrum plus the side info describing it.
#[derive(Clone, Debug)]
struct CodedGranule {
    bits: Vec<bool>,
    part2_3_length: u32,
    big_values: u32,
    global_gain: u32,
    scalefac_compress: u32,
    block_type: u8,
    table_select: [u8; 3],
    region0_count: u32,
    region1_count: u32,
    scalefac_scale: bool,
    count1table_select: bool,
}

/// The stateful encoder core: PCM in, Layer III frames out.
#[derive(Debug)]
pub struct Mp3Encode {
    sample_rate: u32,
    channels: usize,
    bitrate_kbps: u32,
    version: Version,
    mode: ChannelMode,
    analysis: Vec<Analysis>,
    /// Subband slots waiting for their MDCT, per channel.
    slots: Vec<Vec<[f32; 32]>>,
    /// The previous granule's subband slots, for the 36-sample MDCT window.
    history: Vec<[[f32; 32]; 18]>,
    /// Window type chosen for the granule we are holding back.
    pending: Vec<PendingGranule>,
    fft: RealFft<f32>,
    fft_window: Window<f32>,
    psy_history: Vec<Vec<f32>>,
    /// The main-data byte stream and how much of it frames have carried.
    stream: Vec<u8>,
    used: usize,
    /// Stream bytes already assigned to a frame's payload, decided or not.
    assigned: usize,
    /// Stream bytes already written out in a frame.
    written: usize,
    /// Frames whose header and side info are decided but whose payload is
    /// still being filled: the bit reservoir means a frame's payload carries
    /// the *next* frame's granule data, so emission runs one frame behind.
    queued: std::collections::VecDeque<(Vec<u8>, Vec<u8>)>,
    ready: std::collections::VecDeque<Vec<u8>>,
    frames: u32,
}

#[derive(Clone, Debug)]
struct PendingGranule {
    subband: Vec<[f32; 32]>,
    energy: f32,
    threshold: Vec<f32>,
}

impl Mp3Encode {
    /// An encoder for this stream shape. Rates outside 8..48 kHz have no Layer
    /// III frame and are refused by name.
    pub fn new(sample_rate: u32, channels: usize, bitrate_kbps: u32) -> Result<Mp3Encode> {
        let version = match sample_rate {
            32000 | 44100 | 48000 => Version::Mpeg1,
            16000 | 22050 | 24000 => Version::Mpeg2,
            8000 | 11025 | 12000 => Version::Mpeg25,
            other => {
                return Err(Error::unsupported(
                    format!("{other} Hz MPEG audio"),
                    "Layer III frames exist only for 8, 11.025, 12, 16, 22.05, 24, 32, 44.1 and 48 kHz",
                ));
            }
        };
        if channels == 0 || channels > 2 {
            return Err(Error::unsupported(
                format!("{channels}-channel MP3"),
                "Layer III carries one or two channels",
            ));
        }
        Ok(Mp3Encode {
            sample_rate,
            channels,
            bitrate_kbps: snap_bitrate(version, bitrate_kbps),
            version,
            mode: if channels == 2 {
                ChannelMode::JointStereo
            } else {
                ChannelMode::Mono
            },
            analysis: (0..channels).map(|_| Analysis::default()).collect(),
            slots: vec![Vec::new(); channels],
            history: vec![[[0.0; 32]; 18]; channels],
            pending: Vec::new(),
            fft: RealFft::new(1024),
            fft_window: Window::sine(1024),
            psy_history: vec![vec![0.0; 1024]; channels],
            stream: Vec::new(),
            used: 0,
            assigned: 0,
            written: 0,
            queued: std::collections::VecDeque::new(),
            ready: std::collections::VecDeque::new(),
            frames: 0,
        })
    }

    /// The header every frame of this stream carries.
    fn header(&self) -> FrameHeader {
        FrameHeader {
            version: self.version,
            layer: 3,
            crc: false,
            bitrate_kbps: self.bitrate_kbps,
            sample_rate: self.sample_rate,
            padding: false,
            private: false,
            mode: self.mode,
            mode_ext: 0,
            copyright: false,
            original: true,
            emphasis: 0,
        }
    }

    /// Granules per frame for this stream.
    pub fn granules(&self) -> usize {
        self.header().granules()
    }

    /// Samples per channel one frame codes.
    pub fn frame_samples(&self) -> usize {
        self.granules() * 576
    }
}

/// Interleaved PCM in, complete frames out.
#[derive(Debug)]
pub struct Mp3Encoder {
    core: Option<Mp3Encode>,
    config: Mp3EncoderConfig,
    pcm: Vec<f32>,
    frames: std::collections::VecDeque<Vec<u8>>,
    finished: bool,
    header_written: bool,
    total_samples: u64,
}

impl Mp3Encoder {
    /// An encoder that configures itself from the first PCM it is given.
    pub fn new(config: Mp3EncoderConfig) -> Mp3Encoder {
        Mp3Encoder {
            core: None,
            config,
            pcm: Vec::new(),
            frames: std::collections::VecDeque::new(),
            finished: false,
            header_written: false,
            total_samples: 0,
        }
    }

    /// Feeds interleaved `f32` samples in `[-1, 1]`.
    pub fn push_pcm_f32(
        &mut self,
        interleaved: &[f32],
        channels: u16,
        sample_rate: u32,
    ) -> Result<()> {
        if self.core.is_none() {
            let bitrate = match self.config.vbr_quality {
                Some(q) => bitrate_for_quality(q),
                None => self.config.bitrate_kbps.max(8),
            };
            self.core = Some(Mp3Encode::new(sample_rate, usize::from(channels), bitrate)?);
        }
        self.pcm.extend_from_slice(interleaved);
        self.total_samples += (interleaved.len() / usize::from(channels.max(1))) as u64;
        self.drain(false);
        Ok(())
    }

    /// Feeds interleaved 16-bit samples, scaled the way every decoder here
    /// scales them.
    pub fn push_pcm_s16(
        &mut self,
        interleaved: &[i16],
        channels: u16,
        sample_rate: u32,
    ) -> Result<()> {
        let floats: Vec<f32> = interleaved
            .iter()
            .map(|s| f32::from(*s) / 32768.0)
            .collect();
        self.push_pcm_f32(&floats, channels, sample_rate)
    }

    /// Ends the stream: pads the tail to a whole frame and flushes.
    pub fn finish(&mut self) {
        if let Some(core) = &self.core {
            let channels = core.channels;
            // The filterbank runs fifteen slots behind, and the MDCT one
            // granule behind that; feed silence so the last real samples come
            // out the other end.
            let tail = vec![0.0f32; channels * (576 * 2 + 480)];
            self.pcm.extend_from_slice(&tail);
        }
        self.drain(true);
        self.finished = true;
    }

    fn drain(&mut self, flush: bool) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let channels = core.channels;
        let need = core.frame_samples() * channels;
        while self.pcm.len() >= need {
            let block: Vec<f32> = self.pcm.drain(..need).collect();
            core.encode_block(&block);
        }
        if flush {
            if !self.pcm.is_empty() {
                let mut block = std::mem::take(&mut self.pcm);
                block.resize(need, 0.0);
                core.encode_block(&block);
            }
            core.flush_frames();
        }
        while let Some(frame) = core.ready.pop_front() {
            self.frames.push_back(frame);
        }
    }

    /// The next complete frame, [`Error::Eof`] once drained after
    /// [`Mp3Encoder::finish`], [`Error::NeedMore`] before that.
    pub fn next_packet(&mut self) -> Result<Vec<u8>> {
        if !self.header_written && (self.finished || !self.frames.is_empty()) {
            self.header_written = true;
            if let Some(core) = &self.core {
                return Ok(info_frame(core, self.total_samples));
            }
        }
        match self.frames.pop_front() {
            Some(frame) => Ok(frame),
            None if self.finished => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }
}

/// The Xing/Info header frame every player expects first: a silent frame whose
/// main data is the tag, so a decoder that does not know the tag still decodes
/// a legal (silent) frame rather than choking.
fn info_frame(core: &Mp3Encode, samples: u64) -> Vec<u8> {
    let header = core.header();
    let frame_len = header.frame_len().unwrap_or(417);
    let mut out = vec![0u8; frame_len];
    out[..4].copy_from_slice(&header.to_bytes());
    let at = 4 + header.side_info_len();
    let tag: &[u8] = b"Info";
    out[at..at + 4].copy_from_slice(tag);
    out[at + 4..at + 8].copy_from_slice(&1u32.to_be_bytes()); // frames field present
    let frames = samples.div_ceil(header.samples_per_frame() as u64) as u32;
    out[at + 8..at + 12].copy_from_slice(&frames.to_be_bytes());
    out
}

impl Mp3Encode {
    /// Encodes one frame's worth of interleaved PCM.
    fn encode_block(&mut self, pcm: &[f32]) {
        let channels = self.channels;
        let granules = self.granules();
        // Polyphase analysis, one channel at a time.
        for ch in 0..channels {
            let mut mono: Vec<f32> = Vec::with_capacity(pcm.len() / channels);
            for frame in pcm.chunks(channels) {
                mono.push(frame[ch]);
            }
            for chunk in mono.chunks(32) {
                if chunk.len() < 32 {
                    break;
                }
                if let Some(slot) = self.analysis[ch].push(chunk) {
                    self.slots[ch].push(slot);
                }
            }
            self.psy_push(ch, &mono);
        }
        // A granule is 18 slots; hold one back so block switching can look
        // ahead at the next granule's energy.
        let ready = self.slots[0].len() / 18;
        if ready == 0 {
            return;
        }
        for _ in 0..ready {
            for ch in 0..channels {
                let subband: Vec<[f32; 32]> = self.slots[ch].drain(..18).collect();
                let energy: f32 = subband
                    .iter()
                    .flat_map(|slot| slot.iter())
                    .map(|v| v * v)
                    .sum();
                let threshold = self.psy_threshold(ch);
                self.pending.push(PendingGranule {
                    subband,
                    energy,
                    threshold,
                });
            }
        }
        // Emit whole frames while a lookahead granule remains.
        let per_frame = granules * channels;
        while self.pending.len() >= per_frame + channels {
            self.emit_frame();
        }
    }

    /// One FFT per granule of input, kept for the masking model.
    fn psy_push(&mut self, ch: usize, mono: &[f32]) {
        let n = 1024.min(mono.len());
        let history = &mut self.psy_history[ch];
        history.copy_within(n.., 0);
        let keep = 1024 - n;
        history[keep..].copy_from_slice(&mono[mono.len() - n..]);
    }

    /// Allowed noise per long scalefactor band, from the masking model.
    fn psy_threshold(&mut self, ch: usize) -> Vec<f32> {
        let mut block: Vec<f32> = self.psy_history[ch].clone();
        self.fft_window.apply(&mut block);
        let mut spectrum = vec![ec_dsp::Complex::<f32>::ZERO; 513];
        self.fft.forward(&block, &mut spectrum);
        let starts = long_starts(self.sample_rate);
        let mut energy = vec![0.0f32; SFB_LONG];
        for (band, slot) in energy.iter_mut().enumerate() {
            // The 576 coefficients of a granule map onto 512 FFT bins.
            let from = usize::from(starts[band]) * 512 / 576;
            let to = (usize::from(starts[band + 1]) * 512 / 576).max(from + 1);
            let mut sum = 0.0f32;
            let mut log_sum = 0.0f32;
            let mut count = 0.0f32;
            for bin in &spectrum[from..to.min(512)] {
                let power = bin.norm_sqr() + 1e-30;
                sum += power;
                log_sum += power.ln();
                count += 1.0;
            }
            // Spectral flatness as a tonality proxy: a flat band is noise-like
            // and masks less.
            let flatness = if count > 0.0 {
                ((log_sum / count).exp() / (sum / count)).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let offset_db = 6.0 + 15.0 * (1.0 - flatness);
            *slot = sum * 10f32.powf(-offset_db / 10.0);
        }
        // Spreading: masking leaks upward strongly and downward weakly.
        let mut spread = energy.clone();
        for band in 1..SFB_LONG {
            let leak = spread[band - 1] * 0.0032; // about -25 dB per band
            spread[band] = spread[band].max(leak);
        }
        for band in (0..SFB_LONG - 1).rev() {
            let leak = spread[band + 1] * 0.1; // about -10 dB per band
            spread[band] = spread[band].max(leak);
        }
        // Absolute threshold, coarse: hearing is least sensitive at the ends.
        for (band, slot) in spread.iter_mut().enumerate() {
            let rel = band as f32 / SFB_LONG as f32;
            let floor = 1e-9 * (1.0 + 400.0 * (rel - 0.35).max(0.0).powi(2));
            *slot = slot.max(floor);
        }
        spread
    }

    fn emit_frame(&mut self) {
        let channels = self.channels;
        let granules = self.granules();
        let header = self.header();
        let frame_len = header.frame_len().unwrap_or(0);
        let capacity = frame_len - 4 - header.side_info_len();

        // Window types: a granule whose successor is much louder starts the
        // switch, the loud one is short, and the one after stops it.
        let mut types = vec![0u8; granules * channels];
        for gr in 0..granules {
            for ch in 0..channels {
                let index = gr * channels + ch;
                let next = index + channels;
                let here = self.pending[index].energy;
                let there = self.pending.get(next).map_or(here, |g| g.energy);
                if there > here * 8.0 + 1e-6 {
                    types[index] = 1; // start
                }
            }
        }
        for gr in 0..granules {
            for ch in 0..channels {
                let index = gr * channels + ch;
                if index >= channels && types[index - channels] == 1 {
                    types[index] = 2; // short
                } else if index >= 2 * channels && types[index - 2 * channels] == 2 {
                    types[index] = 3; // stop
                }
            }
        }

        let begin = self.assigned - self.used;
        let budget_bits = (self.assigned + capacity - self.used) * 8;
        let mut coded: Vec<CodedGranule> = Vec::with_capacity(granules * channels);
        let share = budget_bits / (granules * channels).max(1);
        for gr in 0..granules {
            for ch in 0..channels {
                let index = gr * channels + ch;
                let granule = self.pending[index].clone();
                let xr = self.mdct(ch, &granule.subband, types[index]);
                let spent: usize = coded.iter().map(|c| c.part2_3_length as usize).sum();
                let left = budget_bits.saturating_sub(spent);
                let target = share.min(left).min(4095);
                coded.push(quantise_granule(
                    &xr,
                    types[index],
                    target as u32,
                    &granule.threshold,
                    self.sample_rate,
                ));
            }
        }
        self.pending.drain(..granules * channels);

        // Assemble the frame: header, side info, and this frame's window on
        // the main-data stream.
        let mut side = BitWriter::new();
        side.write_bits(
            begin as u32,
            if self.version == Version::Mpeg1 { 9 } else { 8 },
        );
        let private = match (self.version == Version::Mpeg1, channels) {
            (true, 1) => 5,
            (true, _) => 3,
            (false, 1) => 1,
            (false, _) => 2,
        };
        side.write_bits(0, private);
        if self.version == Version::Mpeg1 {
            side.write_bits(0, 4 * channels as u32); // scfsi: never reuse
        }
        for granule in &coded {
            side.write_bits(granule.part2_3_length, 12);
            side.write_bits(granule.big_values, 9);
            side.write_bits(granule.global_gain, 8);
            side.write_bits(
                granule.scalefac_compress,
                if self.version == Version::Mpeg1 { 4 } else { 9 },
            );
            if granule.block_type == 0 {
                side.write_bit(false);
                for table in granule.table_select {
                    side.write_bits(u32::from(table), 5);
                }
                side.write_bits(granule.region0_count, 4);
                side.write_bits(granule.region1_count, 3);
            } else {
                side.write_bit(true);
                side.write_bits(u32::from(granule.block_type), 2);
                side.write_bit(false); // mixed_block_flag
                for table in &granule.table_select[..2] {
                    side.write_bits(u32::from(*table), 5);
                }
                side.write_bits(0, 9); // subblock_gain
            }
            if self.version == Version::Mpeg1 {
                side.write_bit(false); // preflag
            }
            side.write_bit(granule.scalefac_scale);
            side.write_bit(granule.count1table_select);
        }
        side.align_to_byte();
        let side_bytes = side.into_bytes();
        debug_assert_eq!(side_bytes.len(), header.side_info_len());

        // Append the granule data to the stream at `used`, byte aligned.
        let mut main = BitWriter::new();
        for granule in &coded {
            for bit in &granule.bits {
                main.write_bit(*bit);
            }
        }
        main.align_to_byte();
        let main_bytes = main.into_bytes();
        // Stream positions the reservoir cap skipped over are stuffing, not
        // data, so they are zeros rather than a shorter stream.
        if self.stream.len() < self.used {
            self.stream.resize(self.used, 0);
        }
        self.stream.truncate(self.used);
        self.stream.extend_from_slice(&main_bytes);
        self.used += main_bytes.len();

        self.queued
            .push_back((header.to_bytes().to_vec(), side_bytes));
        self.assigned += capacity;
        // A queued frame can only go out once real granule data covers its
        // whole payload; padding it early is what would strand the next
        // frame's `main_data_begin` in bytes that were already written.
        while !self.queued.is_empty() && self.stream.len() >= self.written + capacity {
            let (head, side) = self.queued.pop_front().expect("queue is not empty");
            let mut frame = Vec::with_capacity(frame_len);
            frame.extend_from_slice(&head);
            frame.extend_from_slice(&side);
            frame.extend_from_slice(&self.stream[self.written..self.written + capacity]);
            self.written += capacity;
            self.ready.push_back(frame);
            self.frames += 1;
        }
        // main_data_begin is nine bits, so the reservoir cannot reach further
        // back than 511 bytes; anything older is dropped rather than pointed at.
        if self.assigned > self.used + 511 {
            self.used = self.assigned - 511;
        }
    }

    /// Writes out whatever frames are still queued, padding the stream so the
    /// last frame is complete.
    fn flush_frames(&mut self) {
        let header = self.header();
        let frame_len = header.frame_len().unwrap_or(0);
        let capacity = frame_len - 4 - header.side_info_len();
        while let Some((head, side)) = self.queued.pop_front() {
            if self.stream.len() < self.written + capacity {
                self.stream.resize(self.written + capacity, 0);
            }
            let mut frame = Vec::with_capacity(frame_len);
            frame.extend_from_slice(&head);
            frame.extend_from_slice(&side);
            frame.extend_from_slice(&self.stream[self.written..self.written + capacity]);
            self.written += capacity;
            self.ready.push_back(frame);
            self.frames += 1;
        }
    }

    /// One granule of subband samples to 576 spectral lines.
    fn mdct(&mut self, ch: usize, subband: &[[f32; 32]], block_type: u8) -> Vec<f32> {
        let mut xr = vec![0.0f32; 576];
        let history = self.history[ch];
        for sb in 0..32 {
            // The transform window is the previous granule's 18 samples
            // followed by this granule's, with the frequency inversion the
            // decoder applies undone first.
            let mut input = [0.0f32; 36];
            for t in 0..18 {
                let sign = if sb % 2 == 1 && t % 2 == 1 { -1.0 } else { 1.0 };
                input[t] = history[t][sb] * sign;
                input[18 + t] = subband[t][sb] * sign;
            }
            let out = &mut xr[sb * 18..sb * 18 + 18];
            if block_type == 2 {
                mdct_short(&input, out);
            } else {
                mdct_long(&input, block_type, out);
            }
        }
        if block_type != 2 {
            alias_expand(&mut xr, 32);
        }
        let mut store = [[0.0f32; 32]; 18];
        store[..18].copy_from_slice(&subband[..18]);
        self.history[ch] = store;
        xr
    }
}

/// Forward MDCT, 36 samples to 18 coefficients, scaled so the decoder's
/// unscaled inverse reconstructs the input under overlap-add.
fn mdct_long(input: &[f32; 36], block_type: u8, out: &mut [f32]) {
    let window = &windows()[usize::from(block_type)];
    let mut windowed = [0.0f32; 36];
    for i in 0..36 {
        windowed[i] = input[i] * window[i];
    }
    for (k, slot) in out.iter_mut().enumerate() {
        let mut sum = 0.0f64;
        for (n, value) in windowed.iter().enumerate() {
            let angle = std::f64::consts::PI / 72.0 * ((2 * n + 19) * (2 * k + 1)) as f64;
            sum += f64::from(*value) * angle.cos();
        }
        *slot = (sum / 9.0) as f32;
    }
}

/// Forward MDCT for a short block: three 12-sample transforms, interleaved
/// into the spectrum the way the decoder reads them back.
fn mdct_short(input: &[f32; 36], out: &mut [f32]) {
    let window = &windows()[2];
    for w in 0..3 {
        for k in 0..6 {
            let mut sum = 0.0f64;
            for n in 0..12 {
                let sample = input[6 + 6 * w + n] * window[n];
                let angle = std::f64::consts::PI / 24.0 * ((2 * n + 7) * (2 * k + 1)) as f64;
                sum += f64::from(sample) * angle.cos();
            }
            out[k * 3 + w] = (sum / 3.0) as f32;
        }
    }
}

/// Quantises one granule: the rate loop inside the distortion loop.
fn quantise_granule(
    xr: &[f32],
    block_type: u8,
    target_bits: u32,
    threshold: &[f32],
    sample_rate: u32,
) -> CodedGranule {
    let mut scalefac = [0u8; SFB_LONG];
    let mut best: Option<(CodedGranule, f64)> = None;
    // The distortion loop. Two things end it: every band is under its masking
    // threshold *and* the bits are spent, or no band can be amplified further.
    // Spending the bits matters as much as the threshold does — a granule that
    // stops early leaves the extra bitrate on the floor, which is what makes an
    // encoder's quality flat across 128 and 320 kbit/s.
    for _round in 0..20 {
        let (granule, noise) = code_with(xr, block_type, target_bits, &scalefac, sample_rate);
        let ratios: Vec<f64> = (0..SFB_LONG)
            .map(|b| f64::from(noise[b]) / f64::from(threshold[b]).max(1e-30))
            .collect();
        let score: f64 = ratios.iter().sum();
        let spent = granule.part2_3_length;
        if best.as_ref().is_none_or(|(_, s)| score < *s) {
            best = Some((granule, score));
        }
        if block_type == 2 {
            break; // short blocks carry no long scalefactors here
        }
        if spent * 20 > target_bits * 19 {
            break; // the budget is spent; amplifying now only coarsens
        }
        // Amplify the bands that are worst against their threshold.
        let mut order: Vec<usize> = (0..SFB_LONG).filter(|&b| scalefac[b] < 15).collect();
        order.sort_by(|a, b| ratios[*b].total_cmp(&ratios[*a]));
        let over: Vec<usize> = order.iter().copied().filter(|&b| ratios[b] > 1.0).collect();
        let chosen: Vec<usize> = if over.is_empty() {
            order.iter().copied().take(3).collect()
        } else {
            over
        };
        if chosen.is_empty() {
            break;
        }
        for band in chosen {
            scalefac[band] += 1;
        }
    }
    best.expect("at least one round runs").0
}

/// Quantises with a fixed scalefactor set, running the rate loop on
/// `global_gain`, and reports the noise each band ends up with.
fn code_with(
    xr: &[f32],
    block_type: u8,
    budget_bits: u32,
    scalefac: &[u8; SFB_LONG],
    sample_rate: u32,
) -> (CodedGranule, [f32; SFB_LONG]) {
    let (slen1, slen2, compress) = scalefac_compress(scalefac);
    // The scalefactors come out of the same budget as the spectrum does.
    let scalefac_bits = if block_type == 2 {
        0
    } else {
        slen1 * 11 + slen2 * 10
    };
    let target_bits = budget_bits.saturating_sub(scalefac_bits);
    let peak = xr.iter().fold(0.0f32, |acc, v| acc.max(v.abs()));
    // A starting gain that puts the loudest line near the top of the coded
    // range: |xr| / 2^((g-210)/4) raised to 3/4 should land around 8000.
    let start = if peak > 1e-20 {
        (210.0 + 4.0 * (peak.log2() - 13.0 / 0.75)).round() as i32
    } else {
        210
    };
    let mut gain = start.clamp(0, 255);
    let mut ix = vec![0i32; 576];
    let mut coded;
    // The rate loop runs both ways: down while there are bits to spare, up
    // while the granule is too big for them. A line that saturates the coding
    // range is not "cheap", it is clipped, so that check comes first.
    let saturated = |ix: &[i32]| ix.iter().any(|v| v.abs() > 8191);
    quantise(xr, gain, scalefac, sample_rate, block_type, &mut ix);
    coded = code_spectrum(&ix, block_type, sample_rate);
    loop {
        if (saturated(&ix) || coded.0 > target_bits) && gain < 255 {
            gain += 1;
            quantise(xr, gain, scalefac, sample_rate, block_type, &mut ix);
            coded = code_spectrum(&ix, block_type, sample_rate);
            continue;
        }
        if saturated(&ix) || coded.0 > target_bits || gain == 0 {
            break;
        }
        // Try one step quieter; keep it only if it still fits and stays
        // inside the coding range.
        let mut trial = ix.clone();
        quantise(xr, gain - 1, scalefac, sample_rate, block_type, &mut trial);
        let trial_coded = code_spectrum(&trial, block_type, sample_rate);
        if trial_coded.0 > target_bits || saturated(&trial) {
            break;
        }
        gain -= 1;
        ix = trial;
        coded = trial_coded;
    }
    // Last resort: if even the quietest legal gain will not fit the bits we
    // were given, drop the top of the spectrum until it does. A granule that
    // overruns its budget would corrupt the reservoir for every frame after
    // it, which is far worse than a low-passed one.
    let mut limit = 576usize;
    while coded.0 > target_bits && limit > 4 {
        limit = limit * 3 / 4;
        for slot in ix.iter_mut().skip(limit) {
            *slot = 0;
        }
        coded = code_spectrum(&ix, block_type, sample_rate);
    }
    let (_, big_values, tables, region0, region1, count1_select, bits) = coded;
    let mut noise = [0.0f32; SFB_LONG];
    let starts = long_starts(sample_rate);
    let power = power43();
    for band in 0..SFB_LONG {
        let (from, to) = (usize::from(starts[band]), usize::from(starts[band + 1]));
        let scale = band_gain(gain, scalefac[band], block_type);
        let mut sum = 0.0f32;
        for i in from..to.min(576) {
            let magnitude = power[(ix[i].unsigned_abs() as usize).min(MAX_QUANT)] * scale;
            let reconstructed = if ix[i] < 0 { -magnitude } else { magnitude };
            sum += (xr[i] - reconstructed).powi(2);
        }
        noise[band] = sum;
    }
    let mut out = BitWriter::new();
    for (band, value) in scalefac.iter().enumerate() {
        let slen = if band < 11 { slen1 } else { slen2 };
        if block_type != 2 {
            out.write_bits(u32::from(*value), slen);
        }
    }
    let scalefac_bits = out.bit_len() as u32;
    let mut bits_out: Vec<bool> = Vec::with_capacity((scalefac_bits + bits.len() as u32) as usize);
    let bytes = out.into_bytes();
    for i in 0..scalefac_bits as usize {
        bits_out.push((bytes[i / 8] >> (7 - i % 8)) & 1 == 1);
    }
    bits_out.extend_from_slice(&bits);
    let granule = CodedGranule {
        part2_3_length: bits_out.len() as u32,
        bits: bits_out,
        big_values,
        global_gain: gain as u32,
        scalefac_compress: compress,
        block_type,
        table_select: tables,
        region0_count: region0,
        region1_count: region1,
        scalefac_scale: false,
        count1table_select: count1_select,
    };
    (granule, noise)
}

fn band_gain(gain: i32, scalefac: u8, block_type: u8) -> f32 {
    let base = (gain as f32 - 210.0) * 0.25;
    if block_type == 2 {
        base.exp2()
    } else {
        (base - 0.5 * f32::from(scalefac)).exp2()
    }
}

/// The spectrum as integers, per the decoder's requantisation read backwards.
fn quantise(
    xr: &[f32],
    gain: i32,
    scalefac: &[u8; SFB_LONG],
    sample_rate: u32,
    block_type: u8,
    ix: &mut [i32],
) {
    let starts = long_starts(sample_rate);
    // 22 bands, not 21: the topmost band carries no transmitted scalefactor,
    // but it does carry audio — zeroing it low-passes the encoder at 16 kHz.
    for band in 0..22 {
        let (from, to) = (
            usize::from(starts[band]),
            usize::from(starts[band + 1]).min(576),
        );
        let scalefac = if band < SFB_LONG { scalefac[band] } else { 0 };
        let scale = 1.0 / band_gain(gain, scalefac, block_type);
        for i in from..to {
            let value = (xr[i].abs() * scale).powf(0.75);
            let magnitude = (value + 0.4054).min(MAX_QUANT as f32) as i32;
            ix[i] = if xr[i] < 0.0 { -magnitude } else { magnitude };
        }
    }
}

/// Chooses the region split, the tables and the count1 boundary, and writes the
/// spectrum. Returns the bit cost first so the rate loop can stop early.
#[allow(clippy::type_complexity)]
fn code_spectrum(
    ix: &[i32],
    block_type: u8,
    sample_rate: u32,
) -> (u32, u32, [u8; 3], u32, u32, bool, Vec<bool>) {
    // Trailing zeros are not coded at all; before them, a run of values in
    // -1..=1 is coded four at a time.
    let mut rzero = 576;
    while rzero > 0 && ix[rzero - 1] == 0 {
        rzero -= 1;
    }
    let count1_end = (rzero.div_ceil(4) * 4).min(576);
    let mut count1_start = count1_end;
    while count1_start >= 4
        && ix[count1_start - 4..count1_start]
            .iter()
            .all(|v| v.abs() <= 1)
    {
        count1_start -= 4;
    }
    let big_end = count1_start;

    let starts = long_starts(sample_rate);
    let short = block_type == 2;
    // Region boundaries land on band edges. Splitting the big-value region in
    // three roughly equal parts is what makes the three tables specialise;
    // searching every legal split costs far more than it saves.
    let (region0, region1, bounds) = if short {
        let first = usize::from(short_starts(sample_rate)[3]) * 3;
        (8u32, 12u32, [first.min(big_end), big_end, big_end])
    } else {
        let band_at = |target: usize| -> usize {
            (0..21)
                .min_by_key(|&b| usize::from(starts[b + 1]).abs_diff(target))
                .unwrap_or(7)
        };
        let r0 = band_at(big_end / 3).min(15);
        let r1 = band_at(big_end * 2 / 3).saturating_sub(r0 + 1).min(7);
        let a = usize::from(starts[(r0 + 1).min(21)]).min(big_end);
        let b = usize::from(starts[(r0 + r1 + 2).min(21)]).min(big_end);
        (r0 as u32, r1 as u32, [a, b.max(a), big_end])
    };

    let mut tables = [0u8; 3];
    let mut writer = BitWriter::new();
    let mut from = 0usize;
    for (region, to) in bounds.iter().enumerate() {
        let to = (*to).min(big_end);
        if to > from {
            let select = best_table(ix, from, to);
            tables[region] = select;
            let table = huffman::big_table(usize::from(select)).expect("table exists");
            let mut i = from;
            while i + 1 < to {
                huffman::write_pair(&mut writer, table, ix[i], ix[i + 1]);
                i += 2;
            }
        }
        from = to;
    }
    // count1: try both tables, keep the cheaper.
    let quads: Vec<[i32; 4]> = (big_end..count1_end)
        .step_by(4)
        .filter(|i| i + 4 <= 576)
        .map(|i| [ix[i], ix[i + 1], ix[i + 2], ix[i + 3]])
        .collect();
    let cost = |select: bool| -> u32 {
        quads
            .iter()
            .map(|quad| huffman::quad_bits(select, *quad))
            .sum()
    };
    let count1_select = cost(true) < cost(false);
    for quad in &quads {
        huffman::write_quad(&mut writer, count1_select, *quad);
    }
    let bit_len = writer.bit_len() as usize;
    let bytes = writer.into_bytes();
    let mut bits = Vec::with_capacity(bit_len);
    for i in 0..bit_len {
        bits.push((bytes[i / 8] >> (7 - i % 8)) & 1 == 1);
    }
    (
        bits.len() as u32,
        (big_end / 2) as u32,
        tables,
        region0,
        region1,
        count1_select,
        bits,
    )
}

/// Tables that can code a region whose largest magnitude is `max`, cheapest
/// candidates first. Anything above 15 needs an escape-coded table, and the
/// escape tables differ only in how many linbits they spend.
fn candidates(max: u32) -> &'static [u8] {
    match max {
        0..=1 => &[1, 2, 3, 5, 6, 7, 8, 9],
        2 => &[2, 3, 5, 6, 7, 8, 9, 10],
        3 => &[5, 6, 7, 8, 9, 10, 11, 12],
        4..=5 => &[7, 8, 9, 10, 11, 12, 13, 15],
        6..=7 => &[10, 11, 12, 13, 15],
        8..=15 => &[13, 15, 16, 24],
        16..=16 => &[16, 17, 18, 19, 20, 21, 22, 23, 24],
        17..=18 => &[17, 18, 19, 20, 21, 22, 23, 24, 25],
        19..=30 => &[18, 19, 20, 21, 22, 23, 25, 26, 27],
        31..=78 => &[20, 21, 22, 23, 26, 27, 28, 29],
        79..=270 => &[21, 22, 23, 28, 29, 30, 31],
        271..=1038 => &[22, 23, 30, 31],
        _ => &[23, 31],
    }
}

/// The cheapest table for one region.
fn best_table(ix: &[i32], from: usize, to: usize) -> u8 {
    let max = ix[from..to]
        .iter()
        .map(|v| v.unsigned_abs())
        .max()
        .unwrap_or(0);
    let mut best = (u32::MAX, 0u8);
    for &select in candidates(max) {
        let Ok(table) = huffman::big_table(usize::from(select)) else {
            continue;
        };
        if let Some(cost) = table_cost(ix, from, to, table)
            && cost < best.0
        {
            best = (cost, select);
        }
    }
    if best.1 == 0 {
        // Nothing in the shortlist fits, which only a value above the coding
        // range can cause; fall back to the widest escape table.
        return 23;
    }
    best.1
}

fn table_cost(ix: &[i32], from: usize, to: usize, table: Table) -> Option<u32> {
    let mut sum = 0;
    let mut i = from;
    while i + 1 < to {
        sum += huffman::pair_bits(table, ix[i].unsigned_abs(), ix[i + 1].unsigned_abs())?;
        i += 2;
    }
    Some(sum)
}

/// The `scalefac_compress` index whose two lengths hold these scalefactors.
fn scalefac_compress(scalefac: &[u8; SFB_LONG]) -> (u32, u32, u32) {
    let bits_for = |slice: &[u8]| -> u32 {
        let max = slice.iter().copied().max().unwrap_or(0);
        (0..=4u32).find(|n| max < (1 << n)).unwrap_or(4)
    };
    let need1 = bits_for(&scalefac[..11]);
    let need2 = bits_for(&scalefac[11..]);
    let mut best = (u32::MAX, 0u32, 0u32, 0u32);
    for (index, (slen1, slen2)) in SLEN.iter().enumerate() {
        if *slen1 >= need1 && *slen2 >= need2 {
            let cost = slen1 * 11 + slen2 * 10;
            if cost < best.0 {
                best = (cost, *slen1, *slen2, index as u32);
            }
        }
    }
    (best.1, best.2, best.3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrates_snap_to_legal_values() {
        assert_eq!(snap_bitrate(Version::Mpeg1, 128), 128);
        assert_eq!(snap_bitrate(Version::Mpeg1, 130), 128);
        assert_eq!(snap_bitrate(Version::Mpeg1, 1000), 320);
        assert_eq!(snap_bitrate(Version::Mpeg2, 128), 128);
        assert_eq!(snap_bitrate(Version::Mpeg2, 320), 160);
    }

    #[test]
    fn unsupported_rates_are_refused_by_name() {
        let err = Mp3Encode::new(96000, 2, 128).unwrap_err();
        assert!(format!("{err}").contains("96000 Hz"), "{err}");
    }

    /// The encoder's whole analysis chain against the decoder's whole
    /// synthesis chain, quantisation left out: what comes back is what went
    /// in, delayed by the filterbank.
    #[test]
    fn analysis_chain_round_trips_through_the_decoder() {
        use crate::filterbank::{Imdct, Synthesis, alias_reduce};
        let imdct = Imdct::default();
        let mut analysis = Analysis::default();
        let mut synthesis = Synthesis::default();
        let n = 32 * 32 * 24;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32;
                0.4 * (t * 0.05).sin() + 0.2 * (t * 0.31).cos()
            })
            .collect();
        let mut slots: Vec<[f32; 32]> = Vec::new();
        for chunk in input.chunks(32) {
            if let Some(slot) = analysis.push(chunk) {
                slots.push(slot);
            }
        }
        let granules = slots.len() / 18;
        let mut history = [[0.0f32; 32]; 18];
        let mut overlap = [[0.0f32; 18]; 32];
        let mut output: Vec<f32> = Vec::new();
        for g in 0..granules {
            let granule = &slots[g * 18..g * 18 + 18];
            let mut xr = vec![0.0f32; 576];
            for sb in 0..32 {
                let mut window = [0.0f32; 36];
                for t in 0..18 {
                    let sign = if sb % 2 == 1 && t % 2 == 1 { -1.0 } else { 1.0 };
                    window[t] = history[t][sb] * sign;
                    window[18 + t] = granule[t][sb] * sign;
                }
                mdct_long(&window, 0, &mut xr[sb * 18..sb * 18 + 18]);
            }
            alias_expand(&mut xr, 32);
            history.copy_from_slice(granule);
            // decoder side
            alias_reduce(&mut xr, 32);
            let mut out_slots = [[0.0f32; 32]; 18];
            let mut block = [0.0f32; 36];
            for sb in 0..32 {
                imdct.long(&xr[sb * 18..sb * 18 + 18], 0, &mut block);
                for t in 0..18 {
                    let mut sample = block[t] + overlap[sb][t];
                    overlap[sb][t] = block[18 + t];
                    if sb % 2 == 1 && t % 2 == 1 {
                        sample = -sample;
                    }
                    out_slots[t][sb] = sample;
                }
            }
            let mut pcm = [0.0f32; 32];
            for slot in &out_slots {
                synthesis.slot(slot, &mut pcm);
                output.extend_from_slice(&pcm);
            }
        }
        // Find the delay, then check the reconstruction is faithful. The
        // comparison starts past the filterbank's ramp-up, which is silence
        // sliding into the window rather than anything the transform got wrong.
        let (from, len) = (4096usize, 8192usize);
        let mut best = (f64::MIN, 0usize);
        for delay in 0..output.len().min(2048) {
            if delay + from + len > output.len() || from + len > input.len() {
                break;
            }
            let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
            for i in from..from + len {
                let (x, y) = (output[delay + i] as f64, input[i] as f64);
                ab += x * y;
                aa += x * x;
                bb += y * y;
            }
            let corr = ab / (aa * bb).sqrt().max(1e-30);
            if corr > best.0 {
                best = (corr, delay);
            }
        }
        assert!(
            best.0 > 0.99999,
            "analysis/synthesis correlation {:.6} at delay {}",
            best.0,
            best.1
        );
    }

    /// The forward MDCT and the decoder's inverse are a reconstructing pair:
    /// two overlapped granules of a signal come back as themselves.
    #[test]
    fn mdct_round_trips_under_overlap_add() {
        use crate::filterbank::Imdct;
        let imdct = Imdct::default();
        let signal: Vec<f32> = (0..72)
            .map(|i| ((i as f32) * 0.37).sin() + 0.3 * ((i as f32) * 1.1).cos())
            .collect();
        let mut output = vec![0.0f32; 72];
        let mut previous = [0.0f32; 18];
        for granule in 0..3 {
            let mut input = [0.0f32; 36];
            for (i, slot) in input.iter_mut().enumerate() {
                let index = granule * 18 + i;
                if index < signal.len() {
                    *slot = signal[index];
                }
            }
            let mut spectrum = [0.0f32; 18];
            mdct_long(&input, 0, &mut spectrum);
            let mut block = [0.0f32; 36];
            imdct.long(&spectrum, 0, &mut block);
            for t in 0..18 {
                let index = granule * 18 + t;
                if index < output.len() {
                    output[index] = block[t] + previous[t];
                }
            }
            previous.copy_from_slice(&block[18..]);
        }
        // The first granule has no predecessor, so compare from the second.
        let mut worst = 0.0f32;
        for i in 18..54 {
            worst = worst.max((output[i] - signal[i]).abs());
        }
        println!("mdct round trip worst absolute error {worst:e}");
        for i in 18..54 {
            assert!(
                (output[i] - signal[i]).abs() < 1e-5,
                "sample {i}: {} vs {}",
                output[i],
                signal[i]
            );
        }
    }
}

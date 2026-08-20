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
use crate::tables::{MAX_QUANT, SLEN, long_starts, power43, short_starts, short_widths, windows};
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
    /// Runs true variable bitrate: each frame's granules are quantised to the
    /// masking threshold scaled by this quality (no fixed bit budget), and the
    /// frame then picks the smallest legal bitrate that carries them; the
    /// quality's mean lands near [`bitrate_for_quality`]. Easy frames cost
    /// fewer bits, hard ones more.
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

/// The largest value the 12-bit `part2_3_length` side-info field can carry,
/// and so the most bits one granule may be coded to.
const PART2_3_MAX: u32 = (1 << 12) - 1;
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
    /// When set, frames are coded to a quality target and their bitrate is
    /// chosen per frame; `bitrate_kbps` is then the floor, not the rate.
    vbr_quality: Option<f32>,
    version: Version,
    mode: ChannelMode,
    analysis: Vec<Analysis>,
    /// Subband slots waiting for their MDCT, per channel.
    slots: Vec<Vec<[f32; 32]>>,
    /// The previous granule's subband slots, for the 36-sample MDCT window.
    history: Vec<[[f32; 32]; 18]>,
    /// Window type chosen for the granule we are holding back.
    pending: Vec<PendingGranule>,
    /// The block type each channel's previous granule used. Window switching
    /// is a sequence, and the sequence does not restart at a frame boundary.
    previous_block: Vec<u8>,
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
    /// The third field is the frame's main-data capacity in bytes, which a
    /// VBR frame chooses for itself.
    queued: std::collections::VecDeque<(Vec<u8>, Vec<u8>, usize)>,
    ready: std::collections::VecDeque<Vec<u8>>,
    frames: u32,
    /// Bytes of emitted audio frames (everything after the info header), for
    /// the Xing header's byte count and average bitrate.
    frame_bytes: u64,
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
    pub fn new(
        sample_rate: u32,
        channels: usize,
        bitrate_kbps: u32,
        vbr_quality: Option<f32>,
    ) -> Result<Mp3Encode> {
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
            vbr_quality,
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
            previous_block: vec![0; channels],
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
            frame_bytes: 0,
        })
    }

    /// The header every frame of this stream carries, at `bitrate_kbps`.
    fn header(&self, bitrate_kbps: u32) -> FrameHeader {
        FrameHeader {
            version: self.version,
            layer: 3,
            crc: false,
            bitrate_kbps,
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
        self.header(self.bitrate_kbps).granules()
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
            self.core = Some(Mp3Encode::new(
                sample_rate,
                usize::from(channels),
                bitrate,
                self.config.vbr_quality,
            )?);
            self.pcm.resize(PRIMING * usize::from(channels.max(1)), 0.0);
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
            // The filterbank runs fifteen slots behind, the MDCT one granule
            // behind that, and block switching holds one more granule back to
            // look ahead; feed silence so every real sample comes out the other
            // end rather than being dropped with the pipeline.
            let tail = vec![0.0f32; channels * (576 * 3 + 480)];
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
            core.flush_pending();
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
                return Ok(info_frame(core, self.total_samples, self.finished));
            }
        }
        match self.frames.pop_front() {
            Some(frame) => Ok(frame),
            None if self.finished => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }
}

/// Silence prepended to the first PCM the encoder is given.
///
/// The first MDCT block has no predecessor to overlap with, so the frame it
/// produces cannot reconstruct — the transform's startup, not a coding loss.
/// Priming with a frame of silence puts that region before the audio instead
/// of on top of it, and the delay field below tells a decoder to drop it. It
/// is what makes the first frame of a gapless decode as good as the rest;
/// without it that frame correlates 0.67 against its input while every other
/// frame is above 0.999.
const PRIMING: usize = 1152;

/// Samples of PCM this encoder's pipeline runs ahead of its output: the
/// [`PRIMING`] silence, plus the analysis bank running fifteen slots behind
/// its input and the MDCT one granule behind that, less what the decoder's
/// synthesis gives back. Measured end to end (encode, decode,
/// cross-correlate) as 1728 samples of lag, of which 529 is the decoder delay
/// every LAME-tag consumer adds for itself.
///
/// `encoder_delay_is_what_the_tag_says` is that measurement as a test: change
/// the pipeline and it fails rather than silently mis-stating the tag.
pub(crate) const ENCODER_DELAY: u32 = PRIMING as u32 + 47;

/// The decoder delay a LAME tag consumer adds to the encoder delay. Fixed by
/// the tag's own convention, not by anything here, so nothing in the encoder
/// spends it — `encoder_delay_is_what_the_tag_says` is what it is for.
#[allow(dead_code)]
const DECODER_DELAY: u32 = 529;

/// The nine-byte encoder string in the LAME extension.
///
/// It says `LAME3.100` and not `ec-mp3` on purpose, and this is the one place
/// this crate writes something it is not: ffmpeg (and every player that copied
/// its parser) only reads the delay and padding fields when those first four
/// bytes are `LAME`, `Lavc` or `Lavf`. Writing our own name there produces a
/// tag that parses and is then ignored, which is worse than useless — the
/// gapless information would be present and unusable. The rest of the
/// extension is filled honestly or zeroed.
const LAME_VERSION: &[u8; 9] = b"LAME3.100";

/// The Xing/Info header frame every player expects first: a silent frame whose
/// main data is the tag, so a decoder that does not know the tag still decodes
/// a legal (silent) frame rather than choking.
///
/// Carries frame and byte counts and — the point of it — the encoder delay and
/// padding, which is what lets a decoder hand back exactly the samples that
/// went in. Those two are exact when the caller pushes all its PCM before
/// pulling packets, which is the pattern the family's exporter uses; a caller
/// that interleaves pushes and pulls gets the delay (a constant) right and the
/// counts as of the first pull.
fn info_frame(core: &Mp3Encode, samples: u64, complete: bool) -> Vec<u8> {
    let vbr = core.vbr_quality.is_some();
    let header = core.header(core.bitrate_kbps);
    let frame_len = header.frame_len().unwrap_or(417);
    let mut out = vec![0u8; frame_len];
    out[..4].copy_from_slice(&header.to_bytes());
    let at = 4 + header.side_info_len();
    let spf = header.samples_per_frame() as u64;
    let (frames, padding) = if complete {
        let frames = core.frames;
        let coded = u64::from(frames) * spf;
        let padding = coded.saturating_sub(samples + u64::from(ENCODER_DELAY));
        (frames, padding.min(4095) as u32)
    } else {
        (0, 0)
    };
    // The header's byte count is the whole stream, info frame included.
    // `frame_bytes` sums emitted audio-frame lengths; in CBR that is
    // `frames * frame_len`, reducing to the old `(frames + 1) * frame_len`.
    let bytes = if complete {
        core.frame_bytes + frame_len as u64
    } else {
        0
    };
    // The LAME "bitrate" byte: a VBR stream reports its mean rate, a CBR one
    // its constant rate.
    let bitrate_byte = if vbr && complete && samples > 0 {
        let seconds = samples as f64 / f64::from(core.sample_rate);
        let kbps = bytes as f64 * 8.0 / seconds / 1000.0;
        (kbps.round() as u32).min(255) as u8
    } else {
        core.bitrate_kbps.min(255) as u8
    };

    let mut tag: Vec<u8> = Vec::with_capacity(64);
    if vbr {
        tag.extend_from_slice(b"Xing");
    } else {
        tag.extend_from_slice(b"Info");
    }
    tag.extend_from_slice(&3u32.to_be_bytes()); // frames and bytes present
    tag.extend_from_slice(&frames.to_be_bytes());
    tag.extend_from_slice(&(bytes.min(u64::from(u32::MAX)) as u32).to_be_bytes());
    tag.extend_from_slice(LAME_VERSION);
    // Tag revision (0), VBR flag (0x10) and VBR method (3) when variable.
    tag.push(if vbr { 0x70 } else { 0 });
    tag.push(0); // lowpass in 100 Hz units, unstated
    tag.extend_from_slice(&0u32.to_be_bytes()); // replay gain peak
    tag.extend_from_slice(&0u16.to_be_bytes()); // radio replay gain
    tag.extend_from_slice(&0u16.to_be_bytes()); // audiophile replay gain
    tag.push(0); // encoding flags and ATH type
    tag.push(bitrate_byte);
    let delay_padding = (ENCODER_DELAY << 12) | padding;
    tag.extend_from_slice(&delay_padding.to_be_bytes()[1..]); // 12 bits each
    tag.push(0); // misc
    tag.push(0); // mp3gain
    tag.extend_from_slice(&0u16.to_be_bytes()); // preset and surround info
    tag.extend_from_slice(&0u32.to_be_bytes()); // music length
    tag.extend_from_slice(&0u16.to_be_bytes()); // music CRC
    tag.extend_from_slice(&0u16.to_be_bytes()); // tag CRC
    // A frame too small to hold the whole tag keeps the fields that fit, in
    // order, which is how the parsers read it anyway.
    let room = frame_len - at;
    let take = tag.len().min(room);
    out[at..at + take].copy_from_slice(&tag[..take]);
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
        let vbr = self.vbr_quality;

        // Window types. A granule whose successor is much louder starts the
        // switch, the loud one is short, and the one after stops it — and the
        // sequence carries across frames, because a start window in the last
        // granule of one frame demands a short block in the first granule of
        // the next. Restarting it per frame is what leaves a start window
        // facing a normal one, whose overlap does not reconstruct.
        let mut types = vec![0u8; granules * channels];
        for gr in 0..granules {
            for ch in 0..channels {
                let index = gr * channels + ch;
                let here = self.pending[index].energy;
                let next = self
                    .pending
                    .get(index + channels)
                    .map_or(here, |g| g.energy);
                // Digital silence is not a transient to protect: switching on
                // the way out of it (the priming, or a cut in a timeline) costs
                // the resolution a long block would have spent on the attack
                // itself, and there is no pre-echo to hide when nothing
                // precedes it.
                let attack = here > 1e-9 && next > here * 8.0 + 1e-6;
                let block = match self.previous_block[ch] {
                    1 => 2,
                    2 if attack => 2,
                    2 => 3,
                    _ if attack => 1,
                    _ => 0,
                };
                types[index] = block;
                self.previous_block[ch] = block;
            }
        }

        let begin = self.assigned - self.used;

        // The MDCT and the quality-scaled masking threshold are shared by
        // every bitrate candidate VBR tries below; compute them once.
        let mut xrs: Vec<Vec<f32>> = Vec::with_capacity(granules * channels);
        let mut thresholds: Vec<Vec<f32>> = Vec::with_capacity(granules * channels);
        for gr in 0..granules {
            for ch in 0..channels {
                let index = gr * channels + ch;
                let granule = self.pending[index].clone();
                xrs.push(self.mdct(ch, &granule.subband, types[index]));
                thresholds.push(match vbr {
                    Some(quality) => quality_threshold(&granule.threshold, quality),
                    None => granule.threshold,
                });
            }
        }
        self.pending.drain(..granules * channels);

        // Quantise the granules. CBR codes to a fixed frame budget. VBR tries
        // legal bitrates, coding the whole frame's granules against each one's
        // share with the same distortion loop CBR uses, and keeps the
        // cheapest one whose mean noise-to-mask ratio sits under one.
        let (coded, kbps): (Vec<CodedGranule>, u32) = match vbr {
            Some(_) => {
                let table: &[u32] = if self.version == Version::Mpeg1 {
                    &BITRATES_V1
                } else {
                    &BITRATES_V2
                };
                // Codes the frame at one candidate and reports its mean
                // noise-to-mask ratio; the ratio falls as the bitrate rises,
                // so the cheapest candidate at or under 1.0 is found by
                // bisection over the table (four codings, not fourteen).
                let code_at = |candidate: u32| -> (Vec<CodedGranule>, f64) {
                    let header = self.header(candidate);
                    let capacity = header.frame_len().unwrap_or(0) - 4 - header.side_info_len();
                    let share = ((capacity * 8) / (granules * channels).max(1))
                        .min(PART2_3_MAX as usize) as u32;
                    let mut frame_coded = Vec::with_capacity(granules * channels);
                    let mut ratio = 0.0;
                    for index in 0..granules * channels {
                        let (granule, r) = quantise_granule(
                            &xrs[index],
                            types[index],
                            share,
                            &thresholds[index],
                            self.sample_rate,
                        );
                        ratio += r / (granules * channels) as f64;
                        frame_coded.push(granule);
                    }
                    (frame_coded, ratio)
                };
                let (mut lo, mut hi) = (0usize, table.len() - 1);
                let mut best = code_at(table[hi]);
                let mut best_kbps = table[hi];
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let (coded, ratio) = code_at(table[mid]);
                    if ratio <= 1.0 {
                        best = (coded, ratio);
                        best_kbps = table[mid];
                        hi = mid;
                    } else {
                        lo = mid + 1;
                    }
                }
                (best.0, best_kbps)
            }
            None => {
                let header = self.header(self.bitrate_kbps);
                let frame_len = header.frame_len().unwrap_or(0);
                let capacity = frame_len - 4 - header.side_info_len();
                let budget_bits = (self.assigned + capacity - self.used) * 8;
                let share = budget_bits / (granules * channels).max(1);
                let mut frame_coded = Vec::with_capacity(granules * channels);
                for index in 0..granules * channels {
                    let spent: usize = frame_coded.iter().map(|c: &CodedGranule| c.part2_3_length as usize).sum();
                    let left = budget_bits.saturating_sub(spent);
                    let target = share.min(left).min(4095);
                    let (granule, _) = quantise_granule(
                        &xrs[index],
                        types[index],
                        target as u32,
                        &thresholds[index],
                        self.sample_rate,
                    );
                    frame_coded.push(granule);
                }
                (frame_coded, self.bitrate_kbps)
            }
        };

        let header = self.header(kbps);
        let capacity = header.frame_len().unwrap_or(0) - 4 - header.side_info_len();
        let side_info_len = header.side_info_len();

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
        debug_assert_eq!(side_bytes.len(), side_info_len);

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
            .push_back((header.to_bytes().to_vec(), side_bytes, capacity));
        self.assigned += capacity;
        // A queued frame can only go out once real granule data covers its
        // whole payload; padding it early is what would strand the next
        // frame's `main_data_begin` in bytes that were already written.
        while let Some(&(_, _, front)) = self.queued.front() {
            if self.stream.len() < self.written + front {
                break;
            }
            let (head, side, cap) = self.queued.pop_front().expect("queue is not empty");
            let mut frame = Vec::with_capacity(4 + side_info_len + cap);
            frame.extend_from_slice(&head);
            frame.extend_from_slice(&side);
            frame.extend_from_slice(&self.stream[self.written..self.written + cap]);
            self.written += cap;
            self.frame_bytes += (4 + side_info_len + cap) as u64;
            self.ready.push_back(frame);
            self.frames += 1;
        }
        // main_data_begin is nine bits, so the reservoir cannot reach further
        // back than 511 bytes; anything older is dropped rather than pointed at.
        if self.assigned > self.used + 511 {
            self.used = self.assigned - 511;
        }
    }

    /// Encodes the granules still held back for block-switching lookahead.
    fn flush_pending(&mut self) {
        let per_frame = self.granules() * self.channels;
        while self.pending.len() >= per_frame {
            self.emit_frame();
        }
    }

    /// Writes out whatever frames are still queued, padding the stream so the
    /// last frame is complete.
    fn flush_frames(&mut self) {
        while let Some((head, side, capacity)) = self.queued.pop_front() {
            if self.stream.len() < self.written + capacity {
                self.stream.resize(self.written + capacity, 0);
            }
            let mut frame = Vec::with_capacity(4 + side.len() + capacity);
            frame.extend_from_slice(&head);
            frame.extend_from_slice(&side);
            frame.extend_from_slice(&self.stream[self.written..self.written + capacity]);
            self.written += capacity;
            self.frame_bytes += (4 + side.len() + capacity) as u64;
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

/// Scales the masking threshold by `vbr_quality`. A higher quality lowers the
/// allowed noise (a smaller threshold, so more bands are amplified and the
/// granule costs more bits); a lower quality raises it. 0.5 is neutral.
fn quality_threshold(threshold: &[f32], quality: f32) -> Vec<f32> {
    let q = quality.clamp(0.0, 1.0);
    // The psychoacoustic threshold is in the unnormalised FFT's power units,
    // the quantisation noise in MDCT units, so the 0.5 point carries the unit
    // gap plus the masking margin. corner-cut: -48 dB is the calibration that
    // lands wav16-* VBR at its 192 kbit/s mean; a normalised threshold would
    // make it a plain margin.
    let db = (0.5 - q) * 24.0 - 48.0;
    let scale = 10f32.powf(db / 10.0);
    threshold.iter().map(|&t| t * scale).collect()
}

/// Quantises one granule: the rate loop inside the distortion loop.
fn quantise_granule(
    xr: &[f32],
    block_type: u8,
    target_bits: u32,
    threshold: &[f32],
    sample_rate: u32,
) -> (CodedGranule, f64) {
    let mut scalefac = [0u8; SFB_LONG];
    // Score is the summed noise-to-mask ratio over the bands; a mean ratio
    // of at most one is VBR's signal that this bitrate carried the granule
    // cleanly (strict all-bands-under is vetoed by scalefactor-capped bands).
    let mut best: Option<(CodedGranule, f64, usize)> = None;
    // The distortion loop. Two things end it: every band is under its masking
    // threshold *and* the bits are spent, or no band can be amplified further.
    // Spending the bits matters as much as the threshold does — a granule that
    // stops early leaves the extra bitrate on the floor, which is what makes an
    // encoder's quality flat across 128 and 320 kbit/s.
    // Short blocks carry twelve bands, long ones twenty-one.
    let bands = if block_type == 2 { 12 } else { SFB_LONG };
    let threshold = band_thresholds(threshold, block_type, sample_rate);
    for _round in 0..20 {
        let (granule, noise) = code_with(xr, block_type, target_bits, &scalefac, sample_rate);
        let ratios: Vec<f64> = (0..bands)
            .map(|b| f64::from(noise[b]) / f64::from(threshold[b]).max(1e-30))
            .collect();
        let score: f64 = ratios.iter().sum();
        let spent = granule.part2_3_length;
        let mut order: Vec<usize> = (0..bands)
            .filter(|&b| scalefac[b] < max_scalefac(b, block_type))
            .collect();
        order.sort_by(|a, b| ratios[*b].total_cmp(&ratios[*a]));
        let over: Vec<usize> = order.iter().copied().filter(|&b| ratios[b] > 1.0).collect();
        if best.as_ref().is_none_or(|(_, s, _)| score < *s) {
            best = Some((granule, score, bands));
        }
        if spent * 20 > target_bits * 19 {
            break; // the budget is spent; amplifying now only coarsens
        }
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
    let (granule, score, bands) = best.expect("at least one round runs");
    (granule, score / bands as f64)
}

/// Bitstream order for a short block's spectrum.
///
/// A short granule is coded band by band, window by window, line by line,
/// while the spectrum the transform produces interleaves the three windows on
/// every line — the decoder's reorder step. The encoder has to undo it before
/// Huffman coding; writing spectral order instead costs exactly the same
/// number of bits, lands exactly on `part2_3_length`, and hands the decoder a
/// permuted granule, which comes back as noise.
fn short_reorder(sample_rate: u32, spectral: &[i32]) -> Vec<i32> {
    let widths = short_widths(sample_rate);
    let starts = short_starts(sample_rate);
    let mut out = Vec::with_capacity(576);
    for sfb in 0..13 {
        let (width, start) = (usize::from(widths[sfb]), usize::from(starts[sfb]));
        for window in 0..3 {
            for line in 0..width {
                let position = (start + line) * 3 + window;
                out.push(spectral.get(position).copied().unwrap_or(0));
            }
        }
    }
    out.resize(576, 0);
    out
}

/// The largest scalefactor a band can actually carry.
///
/// `scalefac_compress` names two lengths, and the second partition's is at most
/// three bits — so a band above the split cannot hold more than 7 however much
/// the distortion loop would like to amplify it. Ignoring that ceiling is not a
/// rounding error: the quantiser uses the value it wanted, no length can encode
/// it, and the decoder reconstructs the band with a scalefactor of zero, which
/// turns the granule into noise.
fn max_scalefac(band: usize, block_type: u8) -> u8 {
    let split = if block_type == 2 { 6 } else { 11 };
    if band < split { 15 } else { 7 }
}

/// The masking model works in long bands; a short block's twelve bands are
/// read off it at the same frequencies, so one model serves both.
fn band_thresholds(threshold: &[f32], block_type: u8, sample_rate: u32) -> Vec<f32> {
    if block_type != 2 {
        return threshold.to_vec();
    }
    let long = long_starts(sample_rate);
    let short = short_starts(sample_rate);
    (0..12)
        .map(|band| {
            // A short band's lines sit at three times its index in the spectrum.
            let line = usize::from(short[band]) * 3;
            let long_band = (0..SFB_LONG)
                .rev()
                .find(|&b| line >= usize::from(long[b]))
                .unwrap_or(0);
            // Three windows share the band, so each carries a third of it.
            threshold[long_band] / 3.0
        })
        .collect()
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
    let (slen1, slen2, compress) = scalefac_compress(scalefac, block_type);
    // The scalefactors come out of the same budget as the spectrum does. A
    // short block transmits six bands of each length, once per window.
    let scalefac_bits = if block_type == 2 {
        (slen1 + slen2) * 18
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
    let for_coding = |ix: &[i32]| -> Vec<i32> {
        if block_type == 2 {
            short_reorder(sample_rate, ix)
        } else {
            ix.to_vec()
        }
    };
    quantise(xr, gain, scalefac, sample_rate, block_type, &mut ix);
    coded = code_spectrum(&for_coding(&ix), block_type, sample_rate);
    loop {
        if (saturated(&ix) || coded.0 > target_bits) && gain < 255 {
            gain += 1;
            quantise(xr, gain, scalefac, sample_rate, block_type, &mut ix);
            coded = code_spectrum(&for_coding(&ix), block_type, sample_rate);
            continue;
        }
        if saturated(&ix) || coded.0 > target_bits || gain == 0 {
            break;
        }
        // Try one step quieter; keep it only if it still fits and stays
        // inside the coding range.
        let mut trial = ix.clone();
        quantise(xr, gain - 1, scalefac, sample_rate, block_type, &mut trial);
        let trial_coded = code_spectrum(&for_coding(&trial), block_type, sample_rate);
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
        coded = code_spectrum(&for_coding(&ix), block_type, sample_rate);
    }
    let (_, big_values, tables, region0, region1, count1_select, bits) = coded;
    let mut noise = [0.0f32; SFB_LONG];
    let power = power43();
    let residual = |i: usize, band: usize| -> f32 {
        let scale = band_gain(gain, scalefac[band], block_type);
        let magnitude = power[(ix[i].unsigned_abs() as usize).min(MAX_QUANT)] * scale;
        let reconstructed = if ix[i] < 0 { -magnitude } else { magnitude };
        (xr[i] - reconstructed).powi(2)
    };
    if block_type == 2 {
        for (i, _) in ix.iter().enumerate().take(576) {
            let band = short_band(i, sample_rate);
            noise[band.min(SFB_LONG - 1)] += residual(i, band);
        }
    } else {
        let starts = long_starts(sample_rate);
        for band in 0..SFB_LONG {
            let (from, to) = (usize::from(starts[band]), usize::from(starts[band + 1]));
            let mut sum = 0.0f32;
            for i in from..to.min(576) {
                sum += residual(i, band);
            }
            noise[band] = sum;
        }
    }
    let mut out = BitWriter::new();
    if block_type == 2 {
        // The decoder reads short scalefactors band-major, three windows at a
        // time; ours are one per band, so each is written three times.
        for (band, value) in scalefac.iter().enumerate().take(12) {
            let slen = if band < 6 { slen1 } else { slen2 };
            for _window in 0..3 {
                out.write_bits(u32::from(*value), slen);
            }
        }
    } else {
        for (band, value) in scalefac.iter().enumerate() {
            let slen = if band < 11 { slen1 } else { slen2 };
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

fn band_gain(gain: i32, scalefac: u8, _block_type: u8) -> f32 {
    let base = (gain as f32 - 210.0) * 0.25;
    (base - 0.5 * f32::from(scalefac)).exp2()
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
    let quantise_line = |xr: f32, scale: f32| -> i32 {
        let value = (xr.abs() * scale).powf(0.75);
        let magnitude = (value + 0.4054).min(MAX_QUANT as f32) as i32;
        if xr < 0.0 { -magnitude } else { magnitude }
    };
    if block_type == 2 {
        // Short blocks: twelve bands, each with its own scalefactor shared by
        // the three windows. Without them the quantiser's noise is flat across
        // the spectrum, which is what made a switched granule the worst-coded
        // one in the stream.
        for (i, slot) in ix.iter_mut().enumerate().take(576) {
            let scale = 1.0 / band_gain(gain, scalefac[short_band(i, sample_rate)], block_type);
            *slot = quantise_line(xr[i], scale);
        }
        return;
    }
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
            ix[i] = quantise_line(xr[i], scale);
        }
    }
}

/// The short scalefactor band a spectral line belongs to. Short-block spectra
/// interleave the three windows line by line, so the line index divided by
/// three is the frequency line the band table is indexed by.
fn short_band(index: usize, sample_rate: u32) -> usize {
    let line = index / 3;
    let starts = short_starts(sample_rate);
    (0..13)
        .rev()
        .find(|&band| line >= usize::from(starts[band]))
        .unwrap_or(0)
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
    // Region boundaries land on band edges. A granule that switches windows
    // does not transmit `region0_count`/`region1_count` at all — the decoder
    // derives them (three short bands for a short block, eight long bands
    // otherwise, with region 2 empty) — so the encoder has to use exactly
    // those, not a split of its own. Choosing differently costs no bits and
    // parses as different tables, which is a granule of noise wherever the
    // window switches. Only a normal block gets to pick.
    let (region0, region1, bounds) = match block_type {
        2 => {
            let first = usize::from(short_starts(sample_rate)[3]) * 3;
            (8u32, 12u32, [first.min(big_end), big_end, big_end])
        }
        1 | 3 => {
            let first = usize::from(starts[8]);
            (7u32, 13u32, [first.min(big_end), big_end, big_end])
        }
        _ => {
            // Splitting the big-value region in three roughly equal parts is
            // what makes the three tables specialise; searching every legal
            // split costs far more than it saves.
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
        }
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
fn scalefac_compress(scalefac: &[u8; SFB_LONG], block_type: u8) -> (u32, u32, u32) {
    let bits_for = |slice: &[u8]| -> u32 {
        let max = slice.iter().copied().max().unwrap_or(0);
        (0..=4u32).find(|n| max < (1 << n)).unwrap_or(4)
    };
    // Long blocks split their bands 11/10, short blocks 6/6 per window.
    let (first, second, cost1, cost2) = if block_type == 2 {
        (&scalefac[..6], &scalefac[6..12], 18, 18)
    } else {
        (&scalefac[..11], &scalefac[11..], 11, 10)
    };
    // The table's longest pair is (4, 3), so a caller that respects
    // `max_scalefac` always finds an entry; the clamp is what keeps a caller
    // that does not from silently coding a granule the decoder cannot rebuild.
    let need1 = bits_for(first).min(4);
    let need2 = bits_for(second).min(3);
    let mut best = (u32::MAX, 4u32, 3u32, 15u32);
    for (index, (slen1, slen2)) in SLEN.iter().enumerate() {
        if *slen1 >= need1 && *slen2 >= need2 {
            let cost = slen1 * cost1 + slen2 * cost2;
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

    /// The delay the Info tag states is the delay the pipeline has. Our own
    /// decoder ignores the tag, so what it hands back is the untrimmed stream:
    /// the content starts exactly `ENCODER_DELAY + DECODER_DELAY` samples in.
    #[test]
    fn encoder_delay_is_what_the_tag_says() {
        for (rate, channels) in [(44100u32, 1usize), (48000, 2)] {
            let n = 1152 * 8;
            let pcm: Vec<f32> = (0..n * channels)
                .map(|i| {
                    let t = (i / channels) as f32;
                    0.3 * (t * 0.11).sin() + 0.2 * (t * 0.7).cos()
                })
                .collect();
            let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
                bitrate_kbps: 320,
                vbr_quality: None,
            });
            encoder
                .push_pcm_f32(&pcm, channels as u16, rate)
                .expect("encoder accepts this shape");
            encoder.finish();
            let mut bytes = Vec::new();
            while let Ok(frame) = encoder.next_packet() {
                bytes.extend_from_slice(&frame);
            }
            let mut reader = crate::Mp3Reader::new();
            reader.push(&bytes);
            let mut out: Vec<f32> = Vec::new();
            for frame in reader.decode_all() {
                out.extend_from_slice(&frame.samples);
            }
            let mut best = (f64::MIN, 0usize);
            let window = 4096 * channels;
            let mut lag = 0;
            while lag + window * 2 < out.len() {
                let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
                for i in 0..window {
                    let (x, y) = (out[lag + window + i] as f64, pcm[window + i] as f64);
                    ab += x * y;
                    aa += x * x;
                    bb += y * y;
                }
                let corr = ab / (aa * bb).sqrt().max(1e-30);
                if corr > best.0 {
                    best = (corr, lag / channels);
                }
                lag += channels;
            }
            assert!(best.0 > 0.999, "round trip correlation {:.6}", best.0);
            assert_eq!(
                best.1 as u32,
                ENCODER_DELAY + DECODER_DELAY,
                "measured lag at {rate} Hz, {channels} channels"
            );
        }
    }

    /// The whole window sequence — normal, start, short, stop, normal — has to
    /// reconstruct, not just the long blocks. This runs the transform pair
    /// directly, so a failure points at the windows rather than at the
    /// quantiser.
    #[test]
    fn window_sequence_reconstructs() {
        use crate::filterbank::Imdct;
        let imdct = Imdct::default();
        let types = [0u8, 0, 1, 2, 3, 0, 0];
        let signal: Vec<f32> = (0..18 * (types.len() + 2))
            .map(|i| ((i as f32) * 0.37).sin() + 0.3 * ((i as f32) * 1.7).cos())
            .collect();
        let mut output = vec![0.0f32; signal.len()];
        let mut overlap = [0.0f32; 18];
        for (granule, block_type) in types.iter().enumerate() {
            let mut window = [0.0f32; 36];
            for (i, slot) in window.iter_mut().enumerate() {
                let index = granule * 18 + i;
                if index < signal.len() {
                    *slot = signal[index];
                }
            }
            let mut spectrum = [0.0f32; 18];
            if *block_type == 2 {
                mdct_short(&window, &mut spectrum);
            } else {
                mdct_long(&window, *block_type, &mut spectrum);
            }
            let mut block = [0.0f32; 36];
            if *block_type == 2 {
                imdct.short(&spectrum, &mut block);
            } else {
                imdct.long(&spectrum, *block_type, &mut block);
            }
            for t in 0..18 {
                let index = granule * 18 + t;
                if index < output.len() {
                    output[index] = block[t] + overlap[t];
                }
            }
            overlap.copy_from_slice(&block[18..]);
        }
        // The first granule has no predecessor and the last no successor.
        for (granule, block_type) in types.iter().enumerate().take(types.len() - 1).skip(1) {
            let mut error = 0.0f64;
            let mut energy = 0.0f64;
            for i in granule * 18..(granule + 1) * 18 {
                error += (output[i] - signal[i]).powi(2) as f64;
                energy += (signal[i] * signal[i]) as f64;
            }
            let snr = 10.0 * (energy / error.max(1e-30)).log10();
            assert!(
                snr > 60.0,
                "granule {granule} (block type {block_type}) reconstructs at {snr:.1} dB"
            );
        }
    }

    /// A granule that switches to short blocks must come back as itself.
    ///
    /// Short blocks are coded band-major with the three windows interleaved on
    /// every line, which is not the order the transform produces them in. Miss
    /// that permutation and the bit count still lands exactly on
    /// `part2_3_length`, every fixture still decodes, and only the switched
    /// granules come back as noise — so this test drives an attack that forces
    /// the switch and checks the granules around it.
    #[test]
    fn switched_granules_reconstruct() {
        let rate = 44100u32;
        let n = 1152 * 12;
        let attack = 1152 * 6;
        let pcm: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32;
                let tone = 0.5 * (t * 0.21).sin() + 0.3 * (t * 0.93).cos();
                // Six frames of near-silence, then full level: an attack no
                // block-switching encoder can ignore.
                if i < attack { tone * 0.002 } else { tone }
            })
            .collect();
        let mut encoder = Mp3Encoder::new(Mp3EncoderConfig {
            bitrate_kbps: 320,
            vbr_quality: None,
        });
        encoder
            .push_pcm_f32(&pcm, 1, rate)
            .expect("encoder accepts this");
        encoder.finish();
        let mut bytes = Vec::new();
        while let Ok(frame) = encoder.next_packet() {
            bytes.extend_from_slice(&frame);
        }
        let mut reader = crate::Mp3Reader::new();
        reader.push(&bytes);
        let mut out: Vec<f32> = Vec::new();
        for frame in reader.decode_all() {
            out.extend_from_slice(&frame.samples);
        }
        let lag = (ENCODER_DELAY + DECODER_DELAY) as usize;
        // Every granule from the attack onward, where the switch happens.
        let mut worst = (1.0f64, 0usize);
        let mut granule = attack / 576;
        while (granule + 1) * 576 + lag <= out.len() && (granule + 1) * 576 <= pcm.len() {
            let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
            for i in granule * 576..(granule + 1) * 576 {
                let (x, y) = (out[i + lag] as f64, pcm[i] as f64);
                ab += x * y;
                aa += x * x;
                bb += y * y;
            }
            let corr = ab / (aa * bb).sqrt().max(1e-30);
            if corr < worst.0 {
                worst = (corr, granule);
            }
            granule += 1;
        }
        assert!(
            worst.0 > 0.99,
            "granule {} reconstructs at {:.4}",
            worst.1,
            worst.0
        );
    }

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
        let err = Mp3Encode::new(96000, 2, 128, None).unwrap_err();
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

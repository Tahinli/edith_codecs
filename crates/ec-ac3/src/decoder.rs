//! The public decoder: syncframes in, interleaved `f32` audio frames out.

use ec_core::{
    AudioFrame, AudioParameters, Buf, ChannelLayout, ChannelPosition, CodecId, CodecParameters,
    Decoder, Error, Frame, MediaParameters, Packet, Result, SampleFormat, Timestamp,
};

use crate::bsi::{self, Acmod, Bsi};
use crate::decode::{Core, Syntax};
use crate::eac3;
use crate::syncinfo;
use crate::transform::BLOCK_SAMPLES;

/// Blocks per AC-3 syncframe (A/52 §5.3.3); E-AC-3 states its own count.
const AC3_BLOCKS: usize = 6;

/// How the decoder should fold the coded channels down, if at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Downmix {
    /// Hand out every coded channel in the stream's own layout.
    #[default]
    Native,
    /// A/52 §7.8.2 Lo/Ro stereo downmix.
    Stereo,
    /// The §7.8.2 mono sum of that stereo downmix.
    Mono,
}

/// Decoder knobs. The defaults are what the standard asks a decoder to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Fraction of the `dynrng` compression to apply, `0.0` to `1.0`.
    ///
    /// §7.7.1: "the AC-3 decoder shall, by default, implement the compression
    /// characteristic indicated by the `dynrng` values", so this defaults to
    /// full. Set it to `0.0` for the original dynamic range; the raw words stay
    /// visible either way through [`crate::FrameInfo`].
    pub drc_scale: f32,
    /// Channel folding, off by default.
    pub downmix: Downmix,
    /// Generate the pseudo-random noise the format asks for: dither in place
    /// of zero-bit mantissas (§7.3.4) and the blend that fills the spectral
    /// extension band (§E3.6.4.2.4). On by default, because both are what the
    /// standard asks a decoder to do.
    ///
    /// Neither sequence is specified — §7.3.4 says "any reasonably random
    /// sequence" and §E3.6.4.2.4 just names a zero-mean unit-variance source —
    /// so two conformant decoders differ by exactly this noise and nothing
    /// else. Turning it off is how a comparison against another decoder
    /// isolates everything that *is* specified.
    pub dither: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            drc_scale: 1.0,
            downmix: Downmix::Native,
            dither: true,
        }
    }
}

/// What a decoded frame's header said, for callers that route on it rather
/// than on the samples: rate, layout and the metadata AC-3 carries per frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameInfo {
    /// AC-3 or E-AC-3.
    pub syntax: Syntax,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Full-bandwidth channels, LFE excluded.
    pub nfchans: usize,
    /// LFE present.
    pub lfeon: bool,
    /// Dialogue normalisation in -dB. Surfaced, never applied: the volume
    /// control that has to combine it with the listener's setting is not in
    /// this crate (§7.6).
    pub dialnorm: u8,
    /// Heavy compression word, when the frame carried one (§7.7.2).
    pub compr: Option<u8>,
    /// Centre mix level as a linear gain, when the mode has three fronts.
    pub center_mix_level: Option<f32>,
    /// Surround mix level as a linear gain, when the mode has surrounds.
    pub surround_mix_level: Option<f32>,
    /// Bit stream mode (Table 5.7).
    pub bsmod: u8,
    /// Audio coding mode.
    pub acmod: Acmod,
    /// Samples per channel this frame produced.
    pub samples: usize,
}

/// An AC-3 / E-AC-3 decoder.
///
/// One packet is one syncframe (plus any E-AC-3 substreams that belong to it),
/// which is how every container this family reads stores the format.
pub struct Ac3Decoder {
    core: Core,
    params: CodecParameters,
    options: Options,
    pending: Option<Frame>,
    info: Option<FrameInfo>,
    /// Set by [`Decoder::flush`]; the next drained call reports end of stream.
    flushed: bool,
}

impl Default for Ac3Decoder {
    fn default() -> Ac3Decoder {
        Ac3Decoder::new()
    }
}

impl Ac3Decoder {
    /// A decoder with default [`Options`] and no stream configuration yet;
    /// everything it needs comes from the first syncframe.
    pub fn new() -> Ac3Decoder {
        Ac3Decoder::with_options(Options::default())
    }

    /// A decoder with explicit options.
    pub fn with_options(options: Options) -> Ac3Decoder {
        Ac3Decoder {
            core: Core::new(),
            params: CodecParameters::new(CodecId::Ac3),
            options,
            pending: None,
            info: None,
            flushed: false,
        }
    }

    /// The options in force; changes take effect on the next packet.
    pub fn options_mut(&mut self) -> &mut Options {
        &mut self.options
    }

    /// What the last decoded frame's header said.
    pub fn frame_info(&self) -> Option<&FrameInfo> {
        self.info.as_ref()
    }

    /// Decode one syncframe into interleaved `f32` samples.
    ///
    /// `data` starts at a sync word. Truncated input is [`Error::NeedMore`], a
    /// broken bit stream [`Error::Corrupt`], and a construct this build does
    /// not implement [`Error::Unsupported`] with the reason named — never a
    /// panic, on any input.
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<AudioFrame> {
        let mut samples = Vec::new();
        let (info, consumed) = self.decode_substream(data, &mut samples)?;
        let _ = consumed;
        let layout = self.layout(&info);
        let interleaved = self.fold(samples, &info);
        let channels = layout.channel_count();
        let count = interleaved.len().checked_div(channels).unwrap_or(0);
        self.info = Some(info.clone());
        self.params.media = MediaParameters::Audio(AudioParameters {
            sample_rate: info.sample_rate,
            layout: layout.clone(),
            format: Some(SampleFormat::F32),
            bits_per_sample: None,
        });
        self.params.codec = match info.syntax {
            Syntax::Ac3 => CodecId::Ac3,
            Syntax::Eac3 => CodecId::EAc3,
        };
        let mut bytes = Vec::with_capacity(interleaved.len() * 4);
        for v in &interleaved {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        AudioFrame::try_new(
            SampleFormat::F32,
            false,
            layout,
            info.sample_rate,
            count,
            vec![Buf::from_vec(bytes)],
        )
    }

    /// Decode one independent substream, appending interleaved samples.
    /// Returns the frame's header summary and the bytes it consumed.
    fn decode_substream(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<(FrameInfo, usize)> {
        // Six bytes, not five: the sixth is where both syntaxes put bsid.
        if data.len() < 6 {
            return Err(Error::NeedMore);
        }
        // The syntax is decided by bsid, which sits five bytes in for AC-3 and
        // two for E-AC-3 — so read it where both agree it can be found: after
        // the sync word, E-AC-3 puts strmtyp/substreamid/frmsiz first, and
        // bsid is at a fixed offset from the frame start in both.
        let bsid = data[5] >> 3;
        let mut r = ec_core::BitReader::new(data);
        if bsid <= 10 {
            let sync = syncinfo::parse_from(&mut r)?;
            let bsi = bsi::parse_from(&mut r)?;
            if data.len() < sync.frame_size {
                return Err(Error::NeedMore);
            }
            self.core.drc_scale = self.options.drc_scale;
            self.core.dither_on = self.options.dither;
            self.core
                .start_frame(Syntax::Ac3, sync.fscod as usize, &bsi);
            for blk in 0..AC3_BLOCKS {
                self.core.block(&mut r, blk, out)?;
            }
            Ok((
                self.info_from(Syntax::Ac3, sync.sample_rate, &bsi, AC3_BLOCKS),
                sync.frame_size,
            ))
        } else {
            let hdr = eac3::bsi::parse_from(&mut r)?;
            if data.len() < hdr.frame_size {
                return Err(Error::NeedMore);
            }
            if hdr.strmtyp == eac3::StreamType::Dependent {
                return Err(Error::unsupported(
                    "E-AC-3 dependent substream as the first substream of a packet",
                    "a dependent substream carries channel extensions for an \
                     independent one that has to be decoded first",
                ));
            }
            self.core.drc_scale = self.options.drc_scale;
            self.core.dither_on = self.options.dither;
            eac3::decode_frame(&mut self.core, &mut r, &hdr, out)?;
            Ok((
                self.info_from(Syntax::Eac3, hdr.sample_rate, &hdr.bsi, hdr.nblocks),
                hdr.frame_size,
            ))
        }
    }

    fn info_from(&self, syntax: Syntax, sample_rate: u32, bsi: &Bsi, nblocks: usize) -> FrameInfo {
        FrameInfo {
            syntax,
            sample_rate,
            nfchans: bsi.nfchans,
            lfeon: bsi.lfeon,
            dialnorm: bsi.dialnorm,
            compr: bsi.compr,
            center_mix_level: bsi.cmixlev.map(bsi::center_mix_level),
            surround_mix_level: bsi.surmixlev.map(bsi::surround_mix_level),
            bsmod: bsi.bsmod,
            acmod: bsi.acmod,
            samples: nblocks * BLOCK_SAMPLES,
        }
    }

    /// The channel layout the coded channels are handed out in.
    fn native_layout(&self, info: &FrameInfo) -> ChannelLayout {
        use ChannelPosition::*;
        let mut positions: Vec<ChannelPosition> = match info.acmod {
            Acmod::DualMono => vec![FrontLeft, FrontRight],
            Acmod::Mono => vec![FrontCenter],
            Acmod::Stereo => vec![FrontLeft, FrontRight],
            Acmod::Surround3_0 => vec![FrontLeft, FrontRight, FrontCenter],
            Acmod::Surround2_1 => vec![FrontLeft, FrontRight, BackCenter],
            Acmod::Surround3_1 => vec![FrontLeft, FrontRight, FrontCenter, BackCenter],
            Acmod::Surround2_2 => vec![FrontLeft, FrontRight, BackLeft, BackRight],
            Acmod::Surround3_2 => vec![FrontLeft, FrontRight, FrontCenter, BackLeft, BackRight],
        };
        if info.lfeon {
            let fronts = match info.acmod {
                Acmod::Mono => 1,
                Acmod::Surround3_0 | Acmod::Surround3_1 | Acmod::Surround3_2 => 3,
                _ => 2,
            };
            positions.insert(fronts.min(positions.len()), Lfe);
        }
        match positions.len() {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            _ if positions == ChannelLayout::Surround5_1.positions() => ChannelLayout::Surround5_1,
            _ => ChannelLayout::Custom(positions),
        }
    }

    fn layout(&self, info: &FrameInfo) -> ChannelLayout {
        match self.options.downmix {
            Downmix::Native => self.native_layout(info),
            Downmix::Stereo => ChannelLayout::Stereo,
            Downmix::Mono => ChannelLayout::Mono,
        }
    }

    /// Apply the requested downmix, if any (§7.8.2).
    fn fold(&self, samples: Vec<f32>, info: &FrameInfo) -> Vec<f32> {
        if self.options.downmix == Downmix::Native {
            return samples;
        }
        let layout = self.native_layout(info);
        let positions = layout.positions().to_vec();
        let channels = positions.len();
        if channels == 0 {
            return samples;
        }
        let clev = info.center_mix_level.unwrap_or(0.707);
        let slev = info.surround_mix_level.unwrap_or(0.707);
        // §7.8.2 with the standard's own coefficients, then one scaling so a
        // full-scale input cannot overflow the downmix — the standard's fixed
        // -7.65 dB would quieten every stereo listener instead.
        let (mut left, mut right) = (Vec::new(), Vec::new());
        for &p in &positions {
            let (l, r) = match p {
                ChannelPosition::FrontLeft => (1.0, 0.0),
                ChannelPosition::FrontRight => (0.0, 1.0),
                ChannelPosition::FrontCenter => (clev, clev),
                ChannelPosition::BackLeft | ChannelPosition::SideLeft => (slev, 0.0),
                ChannelPosition::BackRight | ChannelPosition::SideRight => (0.0, slev),
                // A single surround channel feeds both sides at 0.7 × slev.
                ChannelPosition::BackCenter => (0.7 * slev, 0.7 * slev),
                // The LFE is not part of a §7.8.2 downmix.
                _ => (0.0, 0.0),
            };
            left.push(l);
            right.push(r);
        }
        let peak = left
            .iter()
            .sum::<f32>()
            .max(right.iter().sum::<f32>())
            .max(1.0);
        let frames = samples.len() / channels;
        match self.options.downmix {
            Downmix::Stereo => {
                let mut out = vec![0.0; frames * 2];
                for f in 0..frames {
                    let block = &samples[f * channels..(f + 1) * channels];
                    let (mut lo, mut ro) = (0.0, 0.0);
                    for (ch, &v) in block.iter().enumerate() {
                        lo += v * left[ch];
                        ro += v * right[ch];
                    }
                    out[f * 2] = lo / peak;
                    out[f * 2 + 1] = ro / peak;
                }
                out
            }
            _ => {
                let mut out = vec![0.0; frames];
                for f in 0..frames {
                    let block = &samples[f * channels..(f + 1) * channels];
                    let mut m = 0.0;
                    for (ch, &v) in block.iter().enumerate() {
                        m += v * (left[ch] + right[ch]);
                    }
                    out[f] = m / (2.0 * peak);
                }
                out
            }
        }
    }
}

impl Decoder for Ac3Decoder {
    fn codec_parameters(&self) -> &CodecParameters {
        &self.params
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        let mut frame = self.decode_frame(&packet.data)?;
        frame.pts = packet.pts.map(|ts| Timestamp::new(ts, packet.time_base));
        self.pending = Some(Frame::Audio(frame));
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        match self.pending.take() {
            Some(frame) => Ok(frame),
            None if self.flushed => Err(Error::Eof),
            None => Err(Error::NeedMore),
        }
    }

    fn flush(&mut self) -> Result<()> {
        // Nothing is held back: one packet in, one frame out. The overlap-add
        // tail belongs to the *next* frame, so there is no delayed frame to
        // drain — draining it would emit a half block of silence.
        self.flushed = true;
        Ok(())
    }

    fn reset(&mut self) {
        self.core.reset();
        self.pending = None;
        self.flushed = false;
    }
}

//! The public decoder: syncframes in, interleaved `f32` audio frames out.

use ec_core::{
    AudioFrame, AudioParameters, Buf, ChannelLayout, ChannelPosition, CodecId, CodecParameters,
    Decoder, Error, Frame, MediaParameters, Packet, Result, SampleFormat, Timestamp,
};

use crate::bsi::{self, Acmod, Bsi};
use crate::decode::{Core, Syntax};
use crate::dependent;
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
    /// Full-bandwidth channels of the independent substream, LFE excluded.
    /// Dependent substreams add to the frame's layout, not to this count.
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
/// which is how the containers this family reads usually store the format. A
/// packet holding several syncframes is walked by [`Ac3Decoder::decode_packet`].
pub struct Ac3Decoder {
    core: Core,
    /// One decoder state per dependent substream of the packet, in packet
    /// order: each carries its own blocks' history from frame to frame.
    dependents: Vec<Core>,
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
            dependents: Vec::new(),
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
    ///
    /// The independent substream's channels come first, then the channels of
    /// each E-AC-3 dependent substream that follows it, in packet order. A
    /// dependent substream whose channel map names a speaker this build has no
    /// position for is refused by name (see [`dependent`]). After the
    /// frame, `data` may hold only padding that is not itself a syncframe:
    /// container slack is ignored. A second syncframe after this one is
    /// [`Error::Unsupported`] naming what it is: this call takes one syncframe,
    /// and [`Ac3Decoder::decode_packet`] takes a packet of several.
    pub fn decode_frame(&mut self, data: &[u8]) -> Result<AudioFrame> {
        let syncframe = self.decode_syncframe(data)?;
        reject_tail(&data[syncframe.consumed..])?;
        self.audio_frame(syncframe.info, syncframe.layout, syncframe.interleaved)
    }

    /// Decode a packet holding one or more syncframes into one audio frame.
    ///
    /// Each syncframe (with its E-AC-3 dependent substreams) is decoded in
    /// packet order against the same decoder state, so the output is the
    /// frames' samples back to back, exactly as decoding them one packet at a
    /// time would give. Bytes after the last syncframe that do not begin a
    /// header are padding and ignored. A following syncframe must share the
    /// first one's channel layout, and an E-AC-3 independent substream of
    /// another program (substream id 1 and up) is refused by name: it would be
    /// stream this decoder does not produce.
    pub fn decode_packet(&mut self, data: &[u8]) -> Result<AudioFrame> {
        let mut first = self.decode_syncframe(data)?;
        let mut pos = first.consumed;
        while begins_syncframe(&data[pos..]) {
            let tail = &data[pos..];
            if tail[5] >> 3 > 10 {
                let hdr = eac3::parse_from(&mut ec_core::BitReader::new(tail))?;
                if hdr.substreamid != 0 {
                    return Err(Error::unsupported(
                        "E-AC-3 independent substream of another program in the packet",
                        "programs 2 and up are not decoded; the packet's first \
                         program is the one this decoder produces",
                    ));
                }
            }
            let next = self.decode_syncframe(tail).map_err(|e| match e {
                Error::NeedMore => Error::corrupt("syncframe in the packet tail is truncated"),
                other => other,
            })?;
            if next.layout != first.layout {
                return Err(Error::unsupported(
                    "syncframes with different channel layouts in one packet",
                    "a packet's frames are concatenated into one frame of one layout",
                ));
            }
            first.interleaved.extend(next.interleaved);
            first.info = next.info;
            pos += next.consumed;
        }
        self.audio_frame(first.info, first.layout, first.interleaved)
    }

    /// Decode the syncframe that starts `data`: its independent substream and
    /// the dependent substreams after it, as interleaved samples in the layout
    /// the options ask for. Bytes after it are the caller's to judge.
    fn decode_syncframe(&mut self, data: &[u8]) -> Result<Syncframe> {
        let mut samples = Vec::new();
        let (info, consumed) = self.decode_substream(data, &mut samples)?;
        let (dependents, consumed) = self.decode_dependents(data, consumed, info.samples)?;
        let mut frame = dependent::split(&samples, &self.core_positions(&info));
        for d in dependents {
            frame = dependent::merge(frame, d);
        }
        let positions: Vec<ChannelPosition> = frame.iter().map(|c| c.position).collect();
        let layout = self.layout(&positions);
        let interleaved = self.fold(dependent::interleave(&frame), &info, &positions);
        Ok(Syncframe {
            info,
            layout,
            interleaved,
            consumed,
        })
    }

    /// The audio frame for decoded interleaved samples, and the decoder's
    /// stream parameters updated to match.
    fn audio_frame(
        &mut self,
        info: FrameInfo,
        layout: ChannelLayout,
        interleaved: Vec<f32>,
    ) -> Result<AudioFrame> {
        let channels = layout.channel_count();
        let count = interleaved.len().checked_div(channels).unwrap_or(0);
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
        self.info = Some(info.clone());
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

    /// Decode the dependent substreams that follow the independent one
    /// (Annex E, E.3.8), starting at `pos`. Returns what each one adds and the
    /// offset after the last of them.
    fn decode_dependents(
        &mut self,
        data: &[u8],
        mut pos: usize,
        frames: usize,
    ) -> Result<(Vec<Vec<dependent::Channel>>, usize)> {
        let mut found = Vec::new();
        let mut slot = 0;
        while let Some(hdr) = dependent::peek(&data[pos..]) {
            let body = data
                .get(pos..pos + hdr.frame_size)
                .ok_or_else(|| Error::corrupt("E-AC-3 dependent substream: truncated"))?;
            if hdr.nblocks * BLOCK_SAMPLES != frames {
                return Err(Error::corrupt(format!(
                    "E-AC-3 dependent substream: {} blocks, the independent substream has {}",
                    hdr.nblocks,
                    frames / BLOCK_SAMPLES
                )));
            }
            let channels = hdr.nfchans + usize::from(hdr.bsi.lfeon);
            let positions = dependent::positions(hdr.chanmap, channels)?;
            if self.dependents.len() <= slot {
                self.dependents.push(Core::new());
            }
            let (drc_scale, dither) = (self.options.drc_scale, self.options.dither);
            let core = &mut self.dependents[slot];
            core.drc_scale = drc_scale;
            core.dither_on = dither;
            let mut r = ec_core::BitReader::new(body);
            let own = eac3::parse_from(&mut r)?;
            let mut samples = Vec::new();
            eac3::decode_frame(core, &mut r, &own, &mut samples)?;
            found.push(dependent::split(&samples, &positions));
            slot += 1;
            pos += hdr.frame_size;
        }
        Ok((found, pos))
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

    /// The positions the independent substream's channels fill, in the order
    /// its samples come out.
    fn core_positions(&self, info: &FrameInfo) -> Vec<ChannelPosition> {
        use ChannelPosition::*;
        let mut positions: Vec<ChannelPosition> = match info.acmod {
            Acmod::DualMono => vec![FrontLeft, FrontRight],
            Acmod::Mono => vec![FrontCenter],
            Acmod::Stereo => vec![FrontLeft, FrontRight],
            Acmod::Surround3_0 => vec![FrontLeft, FrontRight, FrontCenter],
            Acmod::Surround2_1 => vec![FrontLeft, FrontRight, BackCenter],
            Acmod::Surround3_1 => vec![FrontLeft, FrontRight, FrontCenter, BackCenter],
            Acmod::Surround2_2 => vec![FrontLeft, FrontRight, BackLeft, BackRight],
            // ffmpeg places AC-3's Ls/Rs at the side positions ("5.1(side)").
            Acmod::Surround3_2 => vec![FrontLeft, FrontRight, FrontCenter, SideLeft, SideRight],
        };
        if info.lfeon {
            let fronts = match info.acmod {
                Acmod::Mono => 1,
                Acmod::Surround3_0 | Acmod::Surround3_1 | Acmod::Surround3_2 => 3,
                _ => 2,
            };
            positions.insert(fronts.min(positions.len()), Lfe);
        }
        positions
    }

    /// The layout of a frame whose channels sit at `positions` (canonical order).
    fn layout(&self, positions: &[ChannelPosition]) -> ChannelLayout {
        match self.options.downmix {
            Downmix::Native => layout_of(positions),
            Downmix::Stereo => ChannelLayout::Stereo,
            Downmix::Mono => ChannelLayout::Mono,
        }
    }

    /// Apply the requested downmix, if any (§7.8.2).
    fn fold(&self, samples: Vec<f32>, info: &FrameInfo, positions: &[ChannelPosition]) -> Vec<f32> {
        if self.options.downmix == Downmix::Native {
            return samples;
        }
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
        for &p in positions {
            let (l, r) = match p {
                ChannelPosition::FrontLeft => (1.0, 0.0),
                ChannelPosition::FrontRight => (0.0, 1.0),
                ChannelPosition::FrontCenter => (clev, clev),
                // Lc/Rc from a dependent substream sit at the -3 dB point that
                // the standard's centre coefficient stands for.
                ChannelPosition::FrontLeftOfCenter => (0.707, 0.0),
                ChannelPosition::FrontRightOfCenter => (0.0, 0.707),
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
        let mut frame = self.decode_packet(&packet.data)?;
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
        for d in &mut self.dependents {
            d.reset();
        }
        self.pending = None;
        self.flushed = false;
    }
}

/// The layout of canonically ordered `positions`: the named layouts where they
/// match, a custom one otherwise.
fn layout_of(positions: &[ChannelPosition]) -> ChannelLayout {
    match positions {
        [ChannelPosition::FrontCenter] => ChannelLayout::Mono,
        [ChannelPosition::FrontLeft, ChannelPosition::FrontRight] => ChannelLayout::Stereo,
        _ if positions == ChannelLayout::Surround5_1.positions() => ChannelLayout::Surround5_1,
        _ if positions == ChannelLayout::Surround7_1.positions() => ChannelLayout::Surround7_1,
        _ => ChannelLayout::Custom(positions.to_vec()),
    }
}

/// One syncframe as decoded: its interleaved samples in the requested layout,
/// the header summary, and the bytes its independent and dependent substreams
/// took.
struct Syncframe {
    info: FrameInfo,
    layout: ChannelLayout,
    interleaved: Vec<f32>,
    consumed: usize,
}

/// Whether `tail` begins a syncframe header: the sync word, then a header that
/// parses as AC-3 or E-AC-3 (the syntax picked by bsid). A `0x0B77`
/// coincidence that does not parse as a header is junk, which is the spec's
/// own allowance, so it does not count.
fn begins_syncframe(tail: &[u8]) -> bool {
    // Both syntaxes announce a frame with 0x0B77, and no header is shorter
    // than six bytes.
    if tail.len() < 6 || u16::from_be_bytes([tail[0], tail[1]]) != syncinfo::SYNCWORD {
        return false;
    }
    if tail[5] >> 3 > 10 {
        eac3::parse_from(&mut ec_core::BitReader::new(tail)).is_ok()
    } else {
        syncinfo::parse(tail).is_ok()
    }
}

/// What the bytes after a frame and its dependent substreams may say. Nothing,
/// or padding that is not itself a syncframe — the slack a container's block
/// may carry around a frame. A tail that *begins* a syncframe is a second
/// independent substream or a second frame: [`Ac3Decoder::decode_frame`] takes
/// one syncframe, so that is a named refusal, and [`Ac3Decoder::decode_packet`]
/// is the call that walks them.
fn reject_tail(tail: &[u8]) -> Result<()> {
    if begins_syncframe(tail) {
        return Err(Error::unsupported(
            "a second syncframe in the tail of the packet",
            "decode_frame takes one syncframe; decode_packet (and send_packet) \
             walks a packet holding several",
        ));
    }
    Ok(())
}

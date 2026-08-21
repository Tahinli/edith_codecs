//! One decoder seat for every audio codec the family carries: packets in,
//! interleaved `f32` out, in film channel order.
//!
//! This is the registry half of the probe. A caller that has a stream from
//! [`crate::Reader`] asks for [`AudioDecoder::new`] and gets either a decoder
//! or an [`ec_core::Error::Unsupported`] that names the codec *and* why nothing
//! here decodes it — a refusal string with no capability behind it is a bug,
//! not a message.

use ec_core::error::{Error, Result};
use ec_core::frame::{Frame, SampleFormat};
use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, Decoder};

/// Which family decoder is doing the work.
enum Inner {
    Flac(Box<ec_flac::FlacDecoder>),
    Mp3(Box<ec_mp3::Mp3Decoder>),
    Vorbis(Box<ec_vorbis::VorbisDecoder>),
    Ac3(Box<ec_ac3::Ac3Decoder>),
    TrueHd(Box<ec_truehd::TrueHdDecoder>),
    Aac(Box<ec_aac::AacDecoder>),
    Alac(Box<ec_alac::AlacDecoder>),
    Opus(Box<ec_opus::MultistreamDecoder>),
    /// PCM needs no decoder, only a reading of the bytes.
    Pcm(CodecId),
}

/// A decoder for one audio stream.
pub struct AudioDecoder {
    inner: Inner,
    codec: CodecId,
    channels: usize,
    sample_rate: u32,
    /// Raw interleaved frames the decoder has produced so far, before any
    /// trim — the running clock the front-offset calculation below measures
    /// against.
    produced: i64,
    /// Frames actually handed out to the caller so far.
    emitted: i64,
    /// `granule - produced` at the first packet whose granule is known: a
    /// Vorbis stream's very first pages account for the encoder's pre-roll in
    /// their granule, so this is the one-time head start (or catch-up) the
    /// decoder's own count needed to line up with the container's.
    ///
    /// This is a front offset only. An *interior* page's granule marks where
    /// the last packet completed *on that page* ends — not a ceiling every
    /// later page's cumulative output must stay under, since packets can span
    /// pages and pages can end mid-packet. Only the first granule (front) and
    /// the last one (tail, at [`Self::flush`]) are trustworthy trim points.
    trim_offset: Option<i64>,
    /// The last granule [`Self::decode`] saw, carried into [`Self::flush`]
    /// for the terminal packet's own trim: the flush call's packet (the
    /// reader's synthetic end-of-stream marker) carries no granule of its own.
    last_granule: Option<i64>,
    /// The most recently decoded packet's own audio, held back one call
    /// rather than handed to the caller straight away: Vorbis's terminal
    /// packet routinely decodes real samples past the container's true final
    /// granule (its own block reaches further than the stream actually
    /// runs), and so does [`Self::flush`]'s un-overlapped tail on top of it.
    /// Once a packet's audio has left this decoder there is nothing left to
    /// trim it from, so the last packet's audio waits here — combined with
    /// the flush tail — for [`Self::flush`] to cut both down to the file's
    /// real length in one go.
    held: Vec<f32>,
}

impl std::fmt::Debug for AudioDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioDecoder")
            .field("codec", &self.codec.name())
            .field("channels", &self.channels)
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

impl AudioDecoder {
    /// A decoder for the stream `params` describes.
    pub fn new(params: &CodecParameters) -> Result<AudioDecoder> {
        let audio = params
            .audio()
            .ok_or_else(|| Error::corrupt("a decoder was asked for on a non-audio stream"))?;
        let channels = audio.layout.channel_count().max(1);
        let sample_rate = audio.sample_rate;
        let extradata = params.extradata.as_deref();
        let inner = match params.codec {
            CodecId::Flac => {
                let mut params = params.clone();
                params.extradata = flac_stream_info(extradata.unwrap_or_default())
                    .map(ec_core::packet::Buf::copy_from_slice);
                Inner::Flac(Box::new(ec_flac::FlacDecoder::new(params)?))
            }
            CodecId::Mp3 => Inner::Mp3(Box::new(ec_mp3::Mp3Decoder::new(params.clone())?)),
            CodecId::Vorbis => {
                let data = extradata.ok_or_else(|| {
                    Error::corrupt("a Vorbis stream with no header triplet in its extradata")
                })?;
                let headers = ec_ogg::xiph_unlace(data)?;
                Inner::Vorbis(Box::new(ec_vorbis::VorbisDecoder::new(&headers)?))
            }
            CodecId::Ac3 | CodecId::EAc3 => Inner::Ac3(Box::new(ec_ac3::Ac3Decoder::new())),
            CodecId::TrueHd => Inner::TrueHd(Box::new(ec_truehd::TrueHdDecoder::new())),
            CodecId::Aac => {
                let decoder = match extradata {
                    Some(asc) => ec_aac::AacDecoder::with_config_bytes(asc)?,
                    // ADTS states its own configuration in every frame.
                    None => ec_aac::AacDecoder::new(),
                };
                Inner::Aac(Box::new(decoder))
            }
            CodecId::Alac => Inner::Alac(Box::new(ec_alac::AlacDecoder::from_parameters(
                params.clone(),
            )?)),
            CodecId::Opus => {
                let head = extradata.unwrap_or_default();
                let (streams, coupled, mapping) = opus_layout(head, channels)?;
                // 48 kHz whatever the container says: it is the only rate Opus
                // decodes at, and the rate its timestamps are in.
                Inner::Opus(Box::new(ec_opus::MultistreamDecoder::try_with_rate(
                    48_000, streams, coupled, &mapping,
                )?))
            }
            codec if is_pcm(codec) => Inner::Pcm(codec),
            codec => {
                return Err(Error::unsupported(
                    format!("the {} track", codec.name()),
                    match codec.media_type() {
                        ec_core::MediaType::Audio => {
                            "no decoder for it exists in this family (yet)"
                        }
                        _ => "this is an audio probe; picture and subtitles decode elsewhere",
                    },
                ));
            }
        };
        Ok(AudioDecoder {
            inner,
            codec: params.codec,
            channels,
            sample_rate: match params.codec {
                CodecId::Opus => 48_000,
                _ => sample_rate,
            },
            produced: 0,
            emitted: 0,
            trim_offset: None,
            last_granule: None,
            held: Vec::new(),
        })
    }

    /// Which codec this is decoding.
    pub fn codec(&self) -> CodecId {
        self.codec
    }

    /// Channels the decoder emits.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Rate the decoder emits at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// One packet in, interleaved `f32` out, normalised to -1.0..=1.0.
    ///
    /// `out` is cleared first: a packet that decodes to nothing (a header
    /// packet, a frame the decoder swallowed) leaves it empty rather than
    /// leaving the caller with the previous packet's audio.
    pub fn decode(&mut self, packet: &Packet, out: &mut Vec<f32>) -> Result<()> {
        out.clear();
        match &mut self.inner {
            Inner::Flac(d) => drain(d.as_mut(), packet, out)?,
            Inner::Mp3(d) => drain(d.as_mut(), packet, out)?,
            Inner::Vorbis(d) => drain(d.as_mut(), packet, out)?,
            Inner::Ac3(d) => drain(d.as_mut(), packet, out)?,
            Inner::TrueHd(d) => drain(d.as_mut(), packet, out)?,
            Inner::Aac(d) => {
                let audio = d.decode(&packet.data, packet.pts)?;
                self.channels = usize::from(audio.channels).max(1);
                if audio.sample_rate > 0 {
                    self.sample_rate = audio.sample_rate;
                }
                out.extend_from_slice(&audio.samples);
            }
            Inner::Alac(d) => {
                let scale = match d.cookie().sample_format() {
                    SampleFormat::S16 => 1.0 / 32768.0,
                    _ => 1.0 / 2147483648.0,
                };
                out.extend(d.decode(&packet.data)?.iter().map(|&s| s as f32 * scale));
            }
            Inner::Opus(d) => {
                let samples = d.decode_packet(&packet.data)?;
                out.extend_from_slice(&samples);
            }
            Inner::Pcm(codec) => pcm(*codec, &packet.data, out),
        }
        self.trim(ec_ogg::granule_of(packet), out);
        // Only Vorbis holds real, un-overlapped state across a packet
        // boundary that a stream-final granule can still need to reach into
        // (see `held`'s own doc); every other codec here releases its audio
        // the moment it is decoded, exactly as before, so a caller that never
        // calls `flush` still gets every packet's worth back.
        if matches!(self.inner, Inner::Vorbis(_)) {
            std::mem::swap(&mut self.held, out);
        }
        self.emitted += out.len() as i64 / self.channels.max(1) as i64;
        Ok(())
    }

    /// The one-off front trim: only the very first packet this decoder ever
    /// sees can carry a legitimate pre-roll offset (`granule` at real stream
    /// position zero). Most containers' first *audio* packet carries no
    /// granule at all — pages tend to end well into the block run — so once
    /// any later packet is the first to reveal one, that granule is an
    /// interior page mark, not a front offset: subtracting it from what has
    /// already been decoded would assign ordinary block-to-block lag to a
    /// pre-roll that was never there, and can trim away a whole packet's
    /// worth of real audio. Every later packet passes through untouched — an
    /// interior page's granule is not a per-page ceiling (see
    /// [`Self::trim_offset`]); only [`Self::flush`]'s tail trim cuts again.
    fn trim(&mut self, granule: Option<i64>, out: &mut Vec<f32>) {
        let channels = self.channels.max(1) as i64;
        let raw_frames = out.len() as i64 / channels;
        if let Some(granule) = granule {
            self.last_granule = Some(granule);
            if self.trim_offset.is_none() {
                if self.produced == 0 {
                    let offset = (granule - raw_frames).max(0);
                    self.trim_offset = Some(offset);
                    if offset > 0 {
                        let drop = (offset.min(raw_frames) * channels) as usize;
                        out.drain(0..drop);
                    }
                } else {
                    // Too late for a real front offset: this stream's first
                    // audio packet already went by with no granule on it.
                    self.trim_offset = Some(0);
                }
            }
        }
        self.produced += raw_frames;
    }

    /// Signal end of stream and take whatever the decoder was holding back.
    ///
    /// Every codec here that overlaps blocks (Vorbis above all: its terminal
    /// block's un-overlapped right half is real audio up to the stream's
    /// final granule, and is otherwise always exactly one hop short) needs
    /// this told to it once the packets have run out, or the last hop never
    /// comes out at all. `out` is cleared first, same as [`Self::decode`].
    ///
    /// [`Self::held`] (the last packet's own audio, never yet released) is
    /// combined with whatever this call's own flush produces, and the pair
    /// are cut down together to the stream's real final granule — the last
    /// [`Self::decode`] call's packet is routinely the one this trim needs to
    /// reach into, and once its audio had left this decoder that was no
    /// longer possible.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<()> {
        out.clear();
        match &mut self.inner {
            Inner::Flac(d) => flush_drain(d.as_mut(), out)?,
            Inner::Mp3(d) => flush_drain(d.as_mut(), out)?,
            Inner::Vorbis(d) => flush_drain(d.as_mut(), out)?,
            Inner::Ac3(d) => flush_drain(d.as_mut(), out)?,
            Inner::TrueHd(d) => flush_drain(d.as_mut(), out)?,
            // AAC, ALAC, Opus and PCM decode a packet at a time with nothing
            // held back across the end of the file.
            Inner::Aac(_) | Inner::Alac(_) | Inner::Opus(_) | Inner::Pcm(_) => {}
        }
        let channels = self.channels.max(1) as i64;
        self.produced += out.len() as i64 / channels;
        let mut combined = std::mem::take(&mut self.held);
        combined.append(out);
        self.trim_tail(&combined, out);
        Ok(())
    }

    /// The terminal trim: the stream's last granule minus the front offset
    /// is the file's true total frame count, so whatever `combined` (the
    /// held-back last packet plus the flush tail) still has past that is
    /// dropped rather than handed to the caller.
    fn trim_tail(&mut self, combined: &[f32], out: &mut Vec<f32>) {
        let channels = self.channels.max(1) as i64;
        let raw_frames = combined.len() as i64 / channels;
        let keep = match self.last_granule {
            Some(granule) => {
                let offset = self.trim_offset.unwrap_or(0);
                let target = (granule - offset).max(0);
                (target - self.emitted).clamp(0, raw_frames)
            }
            None => raw_frames,
        };
        out.extend_from_slice(&combined[..(keep * channels).max(0) as usize]);
        self.emitted += keep;
    }

    /// Drop everything buffered from before a seek.
    pub fn reset(&mut self) {
        match &mut self.inner {
            Inner::Flac(d) => d.reset(),
            Inner::Mp3(d) => d.reset(),
            Inner::Vorbis(d) => d.reset(),
            Inner::Ac3(d) => d.reset(),
            Inner::TrueHd(d) => d.reset(),
            Inner::Aac(d) => **d = ec_aac::AacDecoder::new(),
            Inner::Alac(d) => d.reset(),
            Inner::Opus(d) => d.reset(),
            Inner::Pcm(_) => {}
        }
        self.held.clear();
        self.produced = 0;
        self.emitted = 0;
        self.trim_offset = None;
        self.last_granule = None;
    }
}

/// The bare 34-byte `STREAMINFO` inside whatever a container calls FLAC
/// extradata.
///
/// Three shapes carry the same block: Matroska's `CodecPrivate` is the whole
/// `fLaC` header chain, mp4's `dfLa` is a version word and then the blocks,
/// and Ogg's first packet has a `\x7fFLAC` wrapper in front of that. Rather
/// than encode three offsets, this looks for the one block header that can
/// only be `STREAMINFO`: type 0, length 34.
fn flac_stream_info(data: &[u8]) -> Option<&[u8]> {
    if data.len() == 34 {
        return Some(data);
    }
    for at in 0..data.len().saturating_sub(38).min(16) {
        let head = &data[at..at + 4];
        let len = u32::from_be_bytes([0, head[1], head[2], head[3]]);
        if head[0] & 0x7f == 0 && len == 34 {
            return data.get(at + 4..at + 38);
        }
    }
    // Nothing recognisable: hand the bytes over as they came and let the
    // decoder read the frames' own headers instead.
    None
}

/// True for the PCM codec ids, which are read rather than decoded.
fn is_pcm(codec: CodecId) -> bool {
    matches!(
        codec,
        CodecId::PcmU8
            | CodecId::PcmS16Le
            | CodecId::PcmS16Be
            | CodecId::PcmS24Le
            | CodecId::PcmS32Le
            | CodecId::PcmF32Le
    )
}

/// Push one packet through an [`ec_core::Decoder`] and take every frame it
/// yields, converted to interleaved `f32`.
fn drain(decoder: &mut dyn Decoder, packet: &Packet, out: &mut Vec<f32>) -> Result<()> {
    decoder.send_packet(packet)?;
    drain_frames(decoder, out)
}

/// [`ec_core::Decoder::flush`], then every frame it releases because of it.
fn flush_drain(decoder: &mut dyn Decoder, out: &mut Vec<f32>) -> Result<()> {
    decoder.flush()?;
    drain_frames(decoder, out)
}

/// Every frame currently ready, converted to interleaved `f32`.
fn drain_frames(decoder: &mut dyn Decoder, out: &mut Vec<f32>) -> Result<()> {
    loop {
        match decoder.receive_frame() {
            Ok(Frame::Audio(frame)) => interleave(&frame, out),
            Ok(Frame::Video(_)) => {
                return Err(Error::corrupt("an audio decoder produced a picture"));
            }
            Err(e) if e.is_need_more() || e.is_eof() => return Ok(()),
            Err(e) => return Err(e),
        }
    }
}

/// One decoded frame appended to `out` as interleaved, normalised `f32`.
fn interleave(frame: &ec_core::AudioFrame, out: &mut Vec<f32>) {
    let channels = frame.channels().max(1);
    let width = frame.format.bytes_per_sample();
    let read = |plane: &[u8], i: usize| -> f32 {
        let at = i * width;
        let b = &plane[at..at + width];
        match frame.format {
            SampleFormat::U8 => (f32::from(b[0]) - 128.0) / 128.0,
            SampleFormat::S16 => f32::from(i16::from_ne_bytes([b[0], b[1]])) / 32768.0,
            SampleFormat::S32 => i32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2147483648.0,
            SampleFormat::F32 => f32::from_ne_bytes([b[0], b[1], b[2], b[3]]),
            SampleFormat::F64 => {
                f64::from_ne_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32
            }
        }
    };
    match frame.planar {
        false => {
            let Some(plane) = frame.data.first() else {
                return;
            };
            let n = (plane.len() / width).min(frame.samples * channels);
            out.extend((0..n).map(|i| read(plane, i)));
        }
        true => {
            for i in 0..frame.samples {
                for c in 0..channels {
                    let Some(plane) = frame.data.get(c) else {
                        continue;
                    };
                    if (i + 1) * width <= plane.len() {
                        out.push(read(plane, i));
                    }
                }
            }
        }
    }
}

/// PCM bytes as normalised `f32`.
fn pcm(codec: CodecId, data: &[u8], out: &mut Vec<f32>) {
    match codec {
        CodecId::PcmU8 => out.extend(data.iter().map(|&b| (f32::from(b) - 128.0) / 128.0)),
        CodecId::PcmS16Le => out.extend(
            data.chunks_exact(2)
                .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0),
        ),
        CodecId::PcmS16Be => out.extend(
            data.chunks_exact(2)
                .map(|c| f32::from(i16::from_be_bytes([c[0], c[1]])) / 32768.0),
        ),
        // 24-bit is stored packed, little-endian, and sign-extends from bit 23.
        CodecId::PcmS24Le => out.extend(data.chunks_exact(3).map(|c| {
            let v = i32::from_le_bytes([0, c[0], c[1], c[2]]);
            v as f32 / 2147483648.0
        })),
        CodecId::PcmS32Le => out.extend(
            data.chunks_exact(4)
                .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2147483648.0),
        ),
        CodecId::PcmF32Le => out.extend(
            data.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])),
        ),
        _ => {}
    }
}

/// The stream layout of an Opus track, read off its `OpusHead`.
///
/// Mapping family 0 is the implied mono/stereo layout the header does not
/// spell out; families 1 and 255 carry the table. A header too short to hold
/// one is treated as family 0, which is what a `.opus` file with a truncated
/// head still plays as.
pub fn opus_layout(head: &[u8], channels: usize) -> Result<(usize, usize, Vec<u8>)> {
    let channels = channels.max(1);
    let family = head.get(18).copied().unwrap_or(0);
    if family == 0 || head.len() < 21 {
        let coupled = usize::from(channels >= 2);
        let mapping: Vec<u8> = (0..channels.min(2) as u8).collect();
        return Ok((channels.min(2) - coupled, coupled, mapping));
    }
    let streams = usize::from(head[19]);
    let coupled = usize::from(head[20]);
    let mapping = head
        .get(21..21 + channels)
        .ok_or_else(|| {
            Error::corrupt(format!(
                "an OpusHead claiming {channels} channels with a {}-byte mapping table",
                head.len().saturating_sub(21)
            ))
        })?
        .to_vec();
    Ok((streams, coupled, mapping))
}

/// The encoder delay an Opus track states in its `OpusHead`, in 48 kHz samples.
pub fn opus_pre_skip(head: &[u8]) -> u64 {
    match head.get(10..12) {
        Some(b) => u64::from(u16::from_le_bytes([b[0], b[1]])),
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_layouts_come_off_the_head_or_are_implied() {
        // Family 0 stereo: one coupled stream, no table in the header.
        let head = b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00";
        assert_eq!(opus_layout(head, 2).unwrap(), (1, 1, vec![0, 1]));
        assert_eq!(opus_layout(head, 1).unwrap(), (1, 0, vec![0]));
        assert_eq!(opus_pre_skip(head), 312);

        // Family 1, 5.1: four streams of which two are coupled.
        let mut six = head[..18].to_vec();
        six.extend_from_slice(&[1, 4, 2, 0, 4, 1, 2, 3, 5]);
        assert_eq!(
            opus_layout(&six, 6).unwrap(),
            (4, 2, vec![0, 4, 1, 2, 3, 5])
        );
        // A table too short for the channel count is named, not guessed at.
        assert!(opus_layout(&six[..23], 6).is_err());
    }

    #[test]
    fn pcm_reads_every_width_at_full_scale() {
        let mut out = Vec::new();
        pcm(CodecId::PcmS16Le, &[0x00, 0x80, 0xff, 0x7f], &mut out);
        assert_eq!(out, vec![-1.0, 32767.0 / 32768.0]);
        out.clear();
        pcm(CodecId::PcmU8, &[0, 128, 255], &mut out);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 0.0);
        out.clear();
        // 24-bit sign-extends: 0x800000 is full-scale negative.
        pcm(CodecId::PcmS24Le, &[0x00, 0x00, 0x80], &mut out);
        assert_eq!(out, vec![-1.0]);
    }
}

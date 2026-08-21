//! Codec identities, stream descriptions and the four trait contracts every
//! container and codec crate in the family implements.

use crate::color::ContentLight;
use crate::error::Result;
use crate::frame::{ChannelLayout, ColorInfo, Frame, PixelFormat, SampleFormat};
use crate::packet::{Buf, Packet};
use crate::timebase::{TimeBase, Timestamp};

/// Which kind of stream something belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    /// Pictures.
    Video,
    /// Sound.
    Audio,
    /// Timed text or bitmap overlays.
    Subtitle,
}

/// Every codec the family carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CodecId {
    /// ITU-T H.264 / MPEG-4 AVC.
    H264,
    /// ITU-T H.265 / MPEG-H HEVC.
    H265,
    /// Google VP8 (also the lossy half of WebP).
    Vp8,
    /// Google VP9.
    Vp9,
    /// AOMedia AV1.
    Av1,
    /// MPEG-4 AAC (LC and friends).
    Aac,
    /// Dolby AC-3.
    Ac3,
    /// Dolby Digital Plus (E-AC-3).
    EAc3,
    /// Dolby TrueHD (MLP), including the Atmos substream.
    TrueHd,
    /// DTS and its extensions (DTS-HD, DTS:X).
    Dts,
    /// ALAC.
    Alac,
    /// Xiph FLAC.
    Flac,
    /// MPEG-1/2 Layer III.
    Mp3,
    /// Xiph Opus.
    Opus,
    /// Xiph Vorbis.
    Vorbis,
    /// Unsigned 8-bit PCM.
    PcmU8,
    /// Signed 16-bit little-endian PCM.
    PcmS16Le,
    /// Signed 16-bit big-endian PCM.
    PcmS16Be,
    /// Signed 24-bit little-endian packed PCM.
    PcmS24Le,
    /// Signed 32-bit little-endian PCM.
    PcmS32Le,
    /// 32-bit float little-endian PCM.
    PcmF32Le,
    /// SubRip text.
    Srt,
    /// WebVTT text.
    WebVtt,
    /// Advanced SubStation Alpha (and SSA).
    Ass,
    /// Blu-ray Presentation Graphic Stream bitmaps.
    Pgs,
    /// MPEG-4 timed text (3GPP `tx3g`).
    Tx3g,
}

impl CodecId {
    /// The media type this codec produces.
    pub fn media_type(&self) -> MediaType {
        use CodecId::*;
        match self {
            H264 | H265 | Vp8 | Vp9 | Av1 => MediaType::Video,
            Aac | Ac3 | EAc3 | TrueHd | Dts | Alac | Flac | Mp3 | Opus | Vorbis | PcmU8
            | PcmS16Le | PcmS16Be | PcmS24Le | PcmS32Le | PcmF32Le => MediaType::Audio,
            Srt | WebVtt | Ass | Pgs | Tx3g => MediaType::Subtitle,
        }
    }

    /// Short lowercase name, stable enough for logs and capability tables.
    pub fn name(&self) -> &'static str {
        use CodecId::*;
        match self {
            H264 => "h264",
            H265 => "h265",
            Vp8 => "vp8",
            Vp9 => "vp9",
            Av1 => "av1",
            Aac => "aac",
            Ac3 => "ac3",
            EAc3 => "eac3",
            TrueHd => "truehd",
            Dts => "dts",
            Alac => "alac",
            Flac => "flac",
            Mp3 => "mp3",
            Opus => "opus",
            Vorbis => "vorbis",
            PcmU8 => "pcm_u8",
            PcmS16Le => "pcm_s16le",
            PcmS16Be => "pcm_s16be",
            PcmS24Le => "pcm_s24le",
            PcmS32Le => "pcm_s32le",
            PcmF32Le => "pcm_f32le",
            Srt => "srt",
            WebVtt => "webvtt",
            Ass => "ass",
            Pgs => "pgs",
            Tx3g => "tx3g",
        }
    }
}

/// Video half of [`CodecParameters`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VideoParameters {
    /// Coded width in pixels.
    pub width: u32,
    /// Coded height in pixels.
    pub height: u32,
    /// Decoded pixel layout, when the container or headers state one.
    pub format: Option<PixelFormat>,
    /// Frame rate *as a rate*: `num/den` is frames per second, so NTSC film is
    /// `24000/1001`. Invert it with [`TimeBase::inverse`] for a tick duration.
    pub frame_rate: Option<TimeBase>,
    /// Sample aspect ratio (`num/den`), when not square.
    pub sample_aspect_ratio: Option<TimeBase>,
    /// H.273 colour description.
    pub color: ColorInfo,
    /// How bright the grade says this stream gets: MaxCLL/MaxFALL and the
    /// mastering display, when the container or an HDR SEI stated them. All
    /// [`None`] for SDR, which is what a tone map falls back on.
    pub light: ContentLight,
}

/// Audio half of [`CodecParameters`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioParameters {
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count, order and meaning.
    pub layout: ChannelLayout,
    /// Decoded sample format, when known before the first frame.
    pub format: Option<SampleFormat>,
    /// Coded bit depth, when the codec has one (FLAC 24, ALAC 16/24).
    pub bits_per_sample: Option<u32>,
}

impl Default for AudioParameters {
    fn default() -> Self {
        AudioParameters {
            sample_rate: 0,
            layout: ChannelLayout::Stereo,
            format: None,
            bits_per_sample: None,
        }
    }
}

/// Per-media-type parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaParameters {
    /// Video stream parameters.
    Video(VideoParameters),
    /// Audio stream parameters.
    Audio(AudioParameters),
    /// Subtitle streams carry no dimensions; their setup lives in `extradata`
    /// (the ASS header, the PGS palette, ...).
    Subtitle,
}

/// Everything a decoder needs before it sees its first packet.
#[derive(Debug, Clone, PartialEq)]
pub struct CodecParameters {
    /// Which codec.
    pub codec: CodecId,
    /// Codec-defined setup bytes: avcC, hvcC, av1C, AudioSpecificConfig, the
    /// FLAC STREAMINFO block, the Vorbis header triplet, and so on.
    pub extradata: Option<Buf>,
    /// Media-type-specific parameters.
    pub media: MediaParameters,
}

impl CodecParameters {
    /// Parameters for `codec` with defaults for its media type and no extradata.
    pub fn new(codec: CodecId) -> CodecParameters {
        let media = match codec.media_type() {
            MediaType::Video => MediaParameters::Video(VideoParameters::default()),
            MediaType::Audio => MediaParameters::Audio(AudioParameters::default()),
            MediaType::Subtitle => MediaParameters::Subtitle,
        };
        CodecParameters {
            codec,
            extradata: None,
            media,
        }
    }

    /// Video parameters, when this is a video stream.
    pub fn video(&self) -> Option<&VideoParameters> {
        match &self.media {
            MediaParameters::Video(v) => Some(v),
            _ => None,
        }
    }

    /// Audio parameters, when this is an audio stream.
    pub fn audio(&self) -> Option<&AudioParameters> {
        match &self.media {
            MediaParameters::Audio(a) => Some(a),
            _ => None,
        }
    }
}

/// One stream of a container, as a demuxer reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    /// Index matching [`Packet::stream`].
    pub index: u32,
    /// Base of every timestamp on this stream's packets.
    pub time_base: TimeBase,
    /// Codec and setup data.
    pub params: CodecParameters,
    /// First presentation timestamp, in `time_base` ticks, when known.
    pub start_time: Option<i64>,
    /// Duration in `time_base` ticks, when the container states one.
    pub duration: Option<i64>,
    /// ISO 639-2 language tag, when the container carries one.
    pub language: Option<String>,
    /// Samples the decoder emits before the first audible one: an MP3's gapless
    /// encoder delay, an Opus stream's pre-skip. Zero where a stream has none.
    ///
    /// It is *not* subtracted from [`duration`](Self::duration), because a
    /// caller counting decoded samples has to count these too before it can
    /// drop them -- audible length is `duration - initial_padding`. Trailing
    /// padding is not reported separately: `duration` already ends at the last
    /// audible sample.
    pub initial_padding: u32,
    /// The container **explicitly** marked this stream as the one to play --
    /// Matroska's `FlagDefault`, which is what names the language a dual-audio
    /// remux opens in.
    ///
    /// Explicitly is the whole of it: `FlagDefault` is 1 when the element is
    /// absent, so "flagged" and "eligible" are different questions and only the
    /// first one picks a track. A file where nobody wrote the element has no
    /// flagged stream at all, and its first stream of a kind is its default
    /// ([`crate::registry`] callers, `ec_probe::Reader::default_stream`).
    pub default: bool,
}

impl StreamInfo {
    /// A stream with no timing hints, no language, no padding and no default
    /// flag.
    pub fn new(index: u32, time_base: TimeBase, params: CodecParameters) -> StreamInfo {
        StreamInfo {
            index,
            time_base,
            params,
            start_time: None,
            duration: None,
            language: None,
            initial_padding: 0,
            default: false,
        }
    }
}

/// Where a seek is allowed to land relative to the requested instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeekMode {
    /// Nearest random access point at or before the target. The default: it is
    /// the only mode that lets a decoder reach the target frame exactly, by
    /// decoding forward and discarding.
    #[default]
    SyncBefore,
    /// Nearest random access point at or after the target.
    SyncAfter,
    /// Exact instant, whether or not a random access point sits there; the
    /// caller handles the pre-roll.
    Exact,
}

/// A container reader: streams in, packets out.
pub trait Demuxer: Send {
    /// The streams found while opening the container.
    fn streams(&self) -> &[StreamInfo];

    /// Next packet in storage order, or [`crate::Error::Eof`] at the end.
    fn next_packet(&mut self) -> Result<Packet>;

    /// Position so that reading resumes near `to` on `stream`.
    ///
    /// `to` carries its own [`TimeBase`], so callers never have to know the
    /// stream's base to seek by wall-clock instant.
    fn seek(&mut self, stream: u32, to: Timestamp, mode: SeekMode) -> Result<()>;
}

/// A container writer: streams declared, then packets, then a finish.
pub trait Muxer: Send {
    /// Declare a stream; returns the index that its packets must carry.
    ///
    /// All streams are declared before the first [`Muxer::write_packet`].
    fn add_stream(&mut self, info: StreamInfo) -> Result<u32>;

    /// Write one packet. Its `stream` must be an index from
    /// [`Muxer::add_stream`] and its timestamps are rescaled by the muxer.
    fn write_packet(&mut self, packet: &Packet) -> Result<()>;

    /// Flush indices and trailers. Takes `&mut self` rather than `self` so a
    /// muxer stays usable behind `Box<dyn Muxer>`.
    fn finish(&mut self) -> Result<()>;
}

/// A decoder: packets in, frames out.
///
/// Push/pull rather than `decode(&Packet) -> Vec<Frame>` because the mapping is
/// genuinely not one to one — a parameter-set packet yields nothing, a
/// reordering decoder yields nothing until its DPB fills, an audio packet can
/// yield several blocks — and because end of stream has to be expressible:
/// [`Decoder::flush`] then drains what reorder delay is still holding. The
/// `Vec` shape would allocate per packet and still need a second entry point
/// for the drain.
pub trait Decoder: Send {
    /// The parameters this decoder was configured with, updated in place when
    /// in-band headers change them.
    fn codec_parameters(&self) -> &CodecParameters;

    /// Submit one packet. Call [`Decoder::receive_frame`] until it answers
    /// [`crate::Error::NeedMore`] before submitting the next.
    fn send_packet(&mut self, packet: &Packet) -> Result<()>;

    /// Take one decoded frame; [`crate::Error::NeedMore`] when none is ready,
    /// [`crate::Error::Eof`] once a flushed decoder is drained.
    fn receive_frame(&mut self) -> Result<Frame>;

    /// Signal end of stream: after this, `receive_frame` returns the delayed
    /// frames and then [`crate::Error::Eof`].
    fn flush(&mut self) -> Result<()>;

    /// Drop all buffered state after a seek. Timestamps and references from
    /// before the seek are discarded.
    fn reset(&mut self);
}

/// An encoder: frames in, packets out — the mirror of [`Decoder`].
pub trait Encoder: Send {
    /// Parameters describing the encoder's output, including the `extradata`
    /// a muxer needs. For codecs whose setup data is derived while encoding
    /// (AAC ASC, avcC), this is final after the first
    /// [`Encoder::receive_packet`].
    fn codec_parameters(&self) -> &CodecParameters;

    /// Submit one frame.
    fn send_frame(&mut self, frame: &Frame) -> Result<()>;

    /// Take one encoded packet; [`crate::Error::NeedMore`] when none is ready,
    /// [`crate::Error::Eof`] once a flushed encoder is drained.
    fn receive_packet(&mut self) -> Result<Packet>;

    /// Signal end of input and drain the lookahead.
    fn flush(&mut self) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_codec_id_has_a_media_type_and_name() {
        // The set the family must express, spelled out so a later crate cannot
        // quietly lose one (subtitle packets included: they are Packets, not
        // Frames).
        let all = [
            CodecId::H264,
            CodecId::H265,
            CodecId::Vp8,
            CodecId::Vp9,
            CodecId::Av1,
            CodecId::Aac,
            CodecId::Ac3,
            CodecId::EAc3,
            CodecId::TrueHd,
            CodecId::Dts,
            CodecId::Alac,
            CodecId::Flac,
            CodecId::Mp3,
            CodecId::Opus,
            CodecId::Vorbis,
            CodecId::PcmU8,
            CodecId::PcmS16Le,
            CodecId::PcmS16Be,
            CodecId::PcmS24Le,
            CodecId::PcmS32Le,
            CodecId::PcmF32Le,
            CodecId::Srt,
            CodecId::WebVtt,
            CodecId::Ass,
            CodecId::Pgs,
            CodecId::Tx3g,
        ];
        assert_eq!(all.len(), 26);
        let mut names: Vec<&str> = all.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), all.len(), "codec names must be unique");
        assert_eq!(CodecId::Av1.media_type(), MediaType::Video);
        assert_eq!(CodecId::Opus.media_type(), MediaType::Audio);
        assert_eq!(CodecId::Pgs.media_type(), MediaType::Subtitle);
    }

    #[test]
    fn parameters_default_per_media_type() {
        let v = CodecParameters::new(CodecId::H265);
        assert!(v.video().is_some() && v.audio().is_none());
        let a = CodecParameters::new(CodecId::Opus);
        assert_eq!(a.audio().unwrap().layout, ChannelLayout::Stereo);
        assert!(matches!(
            CodecParameters::new(CodecId::Pgs).media,
            MediaParameters::Subtitle
        ));

        // NTSC frame rate survives as a rate and inverts into a tick duration.
        let mut p = CodecParameters::new(CodecId::H264);
        if let MediaParameters::Video(v) = &mut p.media {
            v.width = 1920;
            v.height = 1080;
            v.frame_rate = Some(TimeBase::new(24_000, 1001));
            v.format = Some(PixelFormat::I420);
        }
        let fr = p.video().unwrap().frame_rate.unwrap();
        assert_eq!(fr.num(), 24_000);
        assert_eq!(fr.inverse(), TimeBase::NTSC_FILM);
    }

    #[test]
    fn stream_info_carries_language_and_timing() {
        let mut s = StreamInfo::new(2, TimeBase::MILLIS, CodecParameters::new(CodecId::Srt));
        s.language = Some("tur".into());
        s.duration = Some(3_600_000);
        assert_eq!(s.params.codec.media_type(), MediaType::Subtitle);
        assert_eq!(s.index, 2);
        assert_eq!(s.language.as_deref(), Some("tur"));
    }

    #[test]
    fn trait_objects_are_send() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<dyn Demuxer>();
        assert_send::<dyn Muxer>();
        assert_send::<dyn Decoder>();
        assert_send::<dyn Encoder>();
        assert_send::<Packet>();
        assert_send::<Frame>();
    }
}

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

/// The turn a container's display matrix asks a player to make so its pictures
/// are seen the way they were shot: quarter turns **clockwise**. A phone's
/// portrait video is the case that names it -- the pixels stay landscape and
/// the container says which way to turn them.
///
/// This is metadata about the *pixels*, not a user's edit, and it is not a
/// request to touch them here: a demuxer reports it and a consumer (the
/// preview, the encoder) decides. A matrix that is not a quarter turn -- a
/// mirror, a shear, a scale, or a 45-degree quirk -- is [`Rotation::Other`]
/// rather than an approximation, so a caller refuses it by name instead of
/// showing the picture wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// No turn: an identity display matrix, or a container that states one.
    #[default]
    None,
    /// A quarter turn clockwise.
    Cw90,
    /// A half turn (two quarter turns clockwise).
    Cw180,
    /// Three quarter turns clockwise, i.e. one quarter counter-clockwise.
    Cw270,
    /// A display matrix that is not a quarter turn: a mirror (a negative
    /// determinant), a shear, an anisotropic scale, or an angle that is not a
    /// multiple of 90 degrees. The four linear terms are carried verbatim so
    /// the reading is representable rather than silently dropped -- they are
    /// the 16.16 fixed-point `a`, `b`, `c`, `d` of an ISO-BMFF `tkhd` matrix
    /// (`matrix[0][0]`, `[0][1]`, `[1][0]`, `[1][1]`).
    Other {
        /// The matrix's `a` term, 16.16 fixed point.
        a: i32,
        /// The matrix's `b` term, 16.16 fixed point.
        b: i32,
        /// The matrix's `c` term, 16.16 fixed point.
        c: i32,
        /// The matrix's `d` term, 16.16 fixed point.
        d: i32,
    },
}

impl Rotation {
    /// The rotation an ISO-BMFF `tkhd` display matrix states, from its linear
    /// 2x2 part read as 16.16 fixed-point `i32`s.
    ///
    /// The matrix transforms a point `(x, y)` to `(a*x + c*y, b*x + d*y)`, so
    /// the columns `(a, b)` and `(c, d)` are the turned axes: a pure rotation
    /// has both columns the same length (isotropic) and one the other's quarter
    /// turn (`a == d`, `b == -c`). Anything else is [`Rotation::Other`].
    ///
    /// The reading is exact integer arithmetic: the four quarter turns are
    /// recognised by an axis landing on an axis (`b == 0` or `a == 0`). An
    /// encoder writes 16.16 quarter turns exactly, so a value within a fraction
    /// of a degree of one that is not exactly one -- a hand-edited or rescaled
    /// matrix -- reads as [`Rotation::Other`] rather than being rounded onto a
    /// turn this decoder would then apply for real.
    pub fn from_matrix(a: i32, b: i32, c: i32, d: i32) -> Rotation {
        // A pure rotation: isotropic (a == d, b == -c) with something to turn.
        if a == d && b == (-c) && (a != 0 || b != 0) {
            // `(a, b)` is the turned x-axis; a quarter turn lands it on an axis.
            return match (a.signum(), b.signum()) {
                (1, 0) => Rotation::None,
                (-1, 0) => Rotation::Cw180,
                (0, 1) => Rotation::Cw90,
                (0, -1) => Rotation::Cw270,
                _ => Rotation::Other { a, b, c, d },
            };
        }
        Rotation::Other { a, b, c, d }
    }

    /// Quarter turns clockwise, where the stream is a quarter turn at all --
    /// `None` for anything [`Rotation::Other`] carries.
    pub fn steps(self) -> Option<u8> {
        match self {
            Rotation::None => Some(0),
            Rotation::Cw90 => Some(1),
            Rotation::Cw180 => Some(2),
            Rotation::Cw270 => Some(3),
            Rotation::Other { .. } => None,
        }
    }

    /// Whether the turned picture's width and height are the coded ones
    /// swapped.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Cw90 | Rotation::Cw270)
    }

    /// Whether there is nothing to do: no turn, the common case of a file that
    /// was shot the way it is stored.
    pub fn is_none(self) -> bool {
        matches!(self, Rotation::None)
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
    /// The turn this stream's display matrix asks for, where the container
    /// states one. [`Rotation::None`] for a container that states none, for an
    /// identity matrix, and for a non-video stream alike -- the common case.
    ///
    /// This is metadata, not an edit: nothing about [`params`](Self::params)
    /// changes because of it, and a caller that wants the displayed picture
    /// (the preview, the encoder) has to make the turn itself.
    pub rotation: Rotation,
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
            rotation: Rotation::None,
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
        assert_eq!(s.rotation, Rotation::None);
    }

    #[test]
    fn quarter_turn_matrices_read_as_typed_rotations() {
        const ONE: i32 = 65_536; // 16.16 unity.
        let cases = [
            // A tkhd matrix and the turn it states, in this crate's clockwise
            // convention. `(0, -1, 1, 0)` is what ffmpeg's
            // `-display_rotation 90` writes: it asks the picture to be turned
            // a quarter counter-clockwise here, which is Cw270.
            ((ONE, 0, 0, ONE), Rotation::None),
            ((0, -ONE, ONE, 0), Rotation::Cw270),
            ((-ONE, 0, 0, -ONE), Rotation::Cw180),
            ((0, ONE, -ONE, 0), Rotation::Cw90),
            // A scaled identity is still no turn: the reading normalises by the
            // matrix's own scale, so a differently-scaled copy reads the same.
            ((2 * ONE, 0, 0, 2 * ONE), Rotation::None),
        ];
        for ((a, b, c, d), want) in cases {
            assert_eq!(Rotation::from_matrix(a, b, c, d), want, "{a} {b} {c} {d}");
        }
        assert_eq!(Rotation::None.steps(), Some(0));
        assert_eq!(Rotation::Cw90.steps(), Some(1));
        assert_eq!(Rotation::Cw180.steps(), Some(2));
        assert_eq!(Rotation::Cw270.steps(), Some(3));
        assert!(Rotation::Cw90.swaps_axes() && Rotation::Cw270.swaps_axes());
        assert!(!Rotation::None.swaps_axes() && !Rotation::Cw180.swaps_axes());
        assert!(Rotation::None.is_none());
    }

    #[test]
    fn a_matrix_that_is_not_a_quarter_turn_is_kept_not_rounded() {
        const ONE: i32 = 65_536;
        // A 45-degree turn: isotropic and pure, but no axis lands on an axis.
        assert_eq!(
            Rotation::from_matrix(46_340, -46_340, 46_340, 46_340),
            Rotation::Other {
                a: 46_340,
                b: -46_340,
                c: 46_340,
                d: 46_340,
            }
        );
        // A mirror (negative determinant), a mirror of a quarter turn (b != -c),
        // a shear (a != d), and the degenerate all-zero matrix: none is a turn.
        for (a, b, c, d) in [
            (ONE, 0, 0, -ONE),
            (0, ONE, ONE, 0),
            (ONE, 0, ONE, ONE),
            (0, 0, 0, 0),
        ] {
            let got = Rotation::from_matrix(a, b, c, d);
            assert!(
                matches!(got, Rotation::Other { .. }),
                "{a} {b} {c} {d} read as {got:?}"
            );
            assert_eq!(got.steps(), None, "Other is not a quarter count");
        }
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

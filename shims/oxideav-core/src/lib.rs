//! `oxideav-core` as edith consumes it, over [`ec_core`].
//!
//! A shim, not a port: it carries the incumbent's package name and version so a
//! swap is a `[patch.crates-io]` line and nothing else, and it exposes exactly
//! the items the replica names — no more, because an item nobody calls is an
//! item nobody can check. The list, and where each is called from, is in the
//! subtitle and export paths of the engine: `Packet::new`, `TimeBase::new`,
//! `Error::NeedMore`, `CodecId`, `CodecParameters::{audio,subtitle}`,
//! `CodecRegistry`, `Decoder`, `Frame`, `Muxer`, `StreamInfo`, `Result`.
//!
//! Where the two IRs differ in *shape* the shim owns the difference:
//! `oxideav`'s codec id is a string and `ec_core`'s is an enum, its frames own
//! their bytes and `ec_core`'s share a refcounted buffer. Those are converted
//! here, once, rather than at every call site of the replica.
//!
//! [`Packet`] and [`TimeBase`] are `ec_core`'s own: their signatures already
//! match what the replica writes, so a re-export is the whole adapter.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;

pub use ec_core::{Packet, TimeBase};

/// What a call into this family can go wrong with.
///
/// `NeedMore` is load-bearing: the replica's AC-3 path treats it as "not an
/// error, feed me another packet" and everything else as a failure.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The decoder wants another packet before it can hand back a frame.
    NeedMore,
    /// End of stream.
    Eof,
    /// Well-formed but not supported.
    Unsupported(String),
    /// Malformed input.
    InvalidData(String),
    /// No codec in the registry claims these parameters.
    CodecNotFound(String),
    /// Underlying I/O.
    Io(std::io::Error),
    /// Anything else.
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NeedMore => write!(f, "more input needed"),
            Error::Eof => write!(f, "end of stream"),
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::InvalidData(what) => write!(f, "invalid data: {what}"),
            Error::CodecNotFound(id) => write!(f, "no codec {id}"),
            Error::Io(e) => write!(f, "{e}"),
            Error::Other(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<ec_core::Error> for Error {
    fn from(e: ec_core::Error) -> Error {
        match e {
            ec_core::Error::NeedMore => Error::NeedMore,
            ec_core::Error::Eof => Error::Eof,
            ec_core::Error::Unsupported { what, why } => {
                Error::Unsupported(format!("{what}: {why}"))
            }
            ec_core::Error::Corrupt { context } => Error::InvalidData(context),
            ec_core::Error::Io(e) => Error::Io(e),
        }
    }
}

/// This family's result.
pub type Result<T> = std::result::Result<T, Error>;

/// A codec's identifier, as a string: `"ac3"`, `"pgs"`, `"vorbis"`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CodecId(pub String);

impl CodecId {
    /// The id named by `s`.
    pub fn new(s: impl Into<String>) -> CodecId {
        CodecId(s.into())
    }

    /// The name itself.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The `ec_core` codec this names, when the family carries it.
    pub fn to_ec(&self) -> Option<ec_core::CodecId> {
        use ec_core::CodecId::*;
        Some(match self.0.as_str() {
            "h264" | "avc1" => H264,
            "h265" | "hevc" => H265,
            "vp8" => Vp8,
            "vp9" => Vp9,
            "av1" => Av1,
            "aac" => Aac,
            "ac3" => Ac3,
            "eac3" => EAc3,
            "alac" => Alac,
            "flac" => Flac,
            "mp3" => Mp3,
            "opus" => Opus,
            "vorbis" => Vorbis,
            "srt" | "subrip" => Srt,
            "webvtt" | "vtt" => WebVtt,
            "ass" | "ssa" => Ass,
            "pgs" => Pgs,
            "tx3g" => Tx3g,
            _ => return None,
        })
    }
}

impl fmt::Display for CodecId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for CodecId {
    fn from(s: &str) -> CodecId {
        CodecId(s.to_owned())
    }
}

impl From<String> for CodecId {
    fn from(s: String) -> CodecId {
        CodecId(s)
    }
}

/// How a codec is named by a container, so a demuxer can map what it read to a
/// registration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CodecTag {
    /// A four-character code, as mp4 sample entries and RIFF write it.
    FourCc([u8; 4]),
    /// An MPEG-4 `ObjectTypeIndication`.
    Mp4ObjectType(u8),
    /// A Matroska `CodecID` string.
    Matroska(String),
}

impl CodecTag {
    /// The four-character code `code`.
    pub fn fourcc(code: &[u8; 4]) -> CodecTag {
        CodecTag::FourCc(*code)
    }

    /// The MPEG-4 object type `oti`.
    pub fn mp4_object_type(oti: u8) -> CodecTag {
        CodecTag::Mp4ObjectType(oti)
    }

    /// The Matroska `CodecID` `id`.
    pub fn matroska(id: impl Into<String>) -> CodecTag {
        CodecTag::Matroska(id.into())
    }
}

/// How samples are laid out in a video frame. Only the format the family's own
/// encoders take and hand out is named; a caller that needs another adds it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Planar 4:2:0 8-bit.
    #[default]
    Yuv420P,
}

/// A codec's string-keyed knobs, parsed by whichever codec declares them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecOptions(HashMap<String, String>);

impl CodecOptions {
    /// The value set for `key`, if any.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// Set `key` to `value`, replacing what was there.
    pub fn set(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.0.insert(key.into(), value.into());
    }
}

/// What kind of stream a set of parameters describes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaType {
    /// Audio samples.
    Audio,
    /// Video pictures.
    Video,
    /// Timed text or subtitle bitmaps.
    Subtitle,
    /// Opaque payload.
    Data,
    /// Not yet determined.
    #[default]
    Unknown,
}

/// Everything a decoder is told before its first packet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodecParameters {
    /// Which codec.
    pub codec_id: CodecId,
    /// Which kind of stream.
    pub media_type: MediaType,
    /// Audio sample rate.
    pub sample_rate: Option<u32>,
    /// Audio channel count — the replica asks for `Some(2)` to get a downmix.
    pub channels: Option<u16>,
    /// Coded width.
    pub width: Option<u32>,
    /// Coded height.
    pub height: Option<u32>,
    /// How a video stream's samples are laid out.
    pub pixel_format: Option<PixelFormat>,
    /// Codec setup bytes.
    pub extradata: Vec<u8>,
    /// Codec-specific knobs, read by whichever codec declares them.
    pub options: CodecOptions,
}

impl CodecParameters {
    /// Audio parameters for `codec_id`.
    pub fn audio(codec_id: CodecId) -> CodecParameters {
        CodecParameters {
            codec_id,
            media_type: MediaType::Audio,
            ..CodecParameters::default()
        }
    }

    /// Video parameters for `codec_id`.
    pub fn video(codec_id: CodecId) -> CodecParameters {
        CodecParameters {
            codec_id,
            media_type: MediaType::Video,
            ..CodecParameters::default()
        }
    }

    /// Subtitle parameters for `codec_id`.
    pub fn subtitle(codec_id: CodecId) -> CodecParameters {
        CodecParameters {
            codec_id,
            media_type: MediaType::Subtitle,
            ..CodecParameters::default()
        }
    }

    /// Data-stream parameters for `codec_id`.
    pub fn data(codec_id: CodecId) -> CodecParameters {
        CodecParameters {
            codec_id,
            media_type: MediaType::Data,
            ..CodecParameters::default()
        }
    }
}

/// One stream of a container, as a muxer is told about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// Index the stream's packets carry.
    pub index: u32,
    /// The unit its timestamps are in.
    pub time_base: TimeBase,
    /// Duration in `time_base` ticks.
    pub duration: Option<i64>,
    /// First timestamp in `time_base` ticks.
    pub start_time: Option<i64>,
    /// What is in it.
    pub params: CodecParameters,
}

/// One decoded picture.
///
/// No dimensions: the incumbent states them on the stream's
/// [`CodecParameters`] and puts only the row stride on the plane, and a codec
/// crate outside this workspace writes the literal (`oxideav-h265`'s decoder
/// does), so the field list is the incumbent's and not ours to improve.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoFrame {
    /// Presentation time in the stream's time base.
    pub pts: Option<i64>,
    /// One plane per component group; packed formats have exactly one.
    pub planes: Vec<VideoPlane>,
}

/// One plane of a [`VideoFrame`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoPlane {
    /// Bytes between the starts of two rows.
    pub stride: usize,
    /// The plane's bytes.
    pub data: Vec<u8>,
}

/// One decoded block of audio.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioFrame {
    /// Frames per channel in this block.
    pub samples: u32,
    /// Presentation time in the stream's time base.
    pub pts: Option<i64>,
    /// Interleaved (one entry) or planar (one entry per channel) bytes.
    pub data: Vec<Vec<u8>>,
}

/// What a decoder hands back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A picture.
    Video(VideoFrame),
    /// An audio block.
    Audio(AudioFrame),
}

impl From<ec_core::Frame> for Frame {
    fn from(frame: ec_core::Frame) -> Frame {
        match frame {
            ec_core::Frame::Video(video) => Frame::Video(VideoFrame {
                pts: video.pts.map(|t| t.ticks),
                planes: video
                    .planes
                    .into_iter()
                    .map(|plane| VideoPlane {
                        stride: plane.stride,
                        data: plane.data.to_vec(),
                    })
                    .collect(),
            }),
            ec_core::Frame::Audio(audio) => Frame::Audio(AudioFrame {
                samples: audio.samples as u32,
                pts: audio.pts.map(|t| t.ticks),
                data: audio.data.iter().map(|buf| buf.to_vec()).collect(),
            }),
        }
    }
}

/// A decoder: packets in, frames out.
pub trait Decoder: Send {
    /// The codec this decodes.
    fn codec_id(&self) -> &CodecId;

    /// Feed one packet.
    fn send_packet(&mut self, packet: &Packet) -> Result<()>;

    /// Take one frame, or [`Error::NeedMore`] when the decoder wants another
    /// packet first.
    fn receive_frame(&mut self) -> Result<Frame>;

    /// Signal end of input, so whatever is held back becomes available through
    /// [`receive_frame`](Decoder::receive_frame) and then [`Error::Eof`].
    ///
    /// The default is for a decoder that holds nothing back — one packet in,
    /// one frame out, which is what the AC-3 path is.
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// An encoder: frames in, packets out.
pub trait Encoder: Send {
    /// The codec this produces.
    fn codec_id(&self) -> &CodecId;

    /// Parameters describing the stream it produces, for the muxer.
    fn output_params(&self) -> &CodecParameters;

    /// Feed one uncompressed frame.
    fn send_frame(&mut self, frame: &Frame) -> Result<()>;

    /// Take one packet, or [`Error::NeedMore`] when the encoder wants another
    /// frame first.
    fn receive_packet(&mut self) -> Result<Packet>;

    /// Signal end of input, so the lookahead drains into
    /// [`receive_packet`](Encoder::receive_packet).
    fn flush(&mut self) -> Result<()>;
}

/// A container writer.
pub trait Muxer: Send {
    /// The format's short name.
    fn format_name(&self) -> &str;

    /// Write whatever precedes the first packet.
    fn write_header(&mut self) -> Result<()>;

    /// Write one packet.
    fn write_packet(&mut self, packet: &Packet) -> Result<()>;

    /// Write whatever follows the last one.
    fn write_trailer(&mut self) -> Result<()>;
}

/// How a codec crate hands out decoders.
pub type DecoderFactory = fn(&CodecParameters) -> Result<Box<dyn Decoder>>;

/// How a codec crate hands out encoders.
pub type EncoderFactory = fn(&CodecParameters) -> Result<Box<dyn Encoder>>;

/// One codec's registration.
#[derive(Clone)]
pub struct CodecInfo {
    /// The id this claims.
    pub id: CodecId,
    /// How to build a decoder for it.
    pub decoder_factory: Option<DecoderFactory>,
    /// How to build an encoder for it.
    pub encoder_factory: Option<EncoderFactory>,
    /// The container-level names that mean this codec.
    pub tags: Vec<CodecTag>,
}

impl CodecInfo {
    /// An empty registration for `id`.
    pub fn new(id: CodecId) -> CodecInfo {
        CodecInfo {
            id,
            decoder_factory: None,
            encoder_factory: None,
            tags: Vec::new(),
        }
    }

    /// The same registration, with a decoder.
    pub fn decoder(mut self, factory: DecoderFactory) -> CodecInfo {
        self.decoder_factory = Some(factory);
        self
    }

    /// The same registration, with an encoder.
    pub fn encoder(mut self, factory: EncoderFactory) -> CodecInfo {
        self.encoder_factory = Some(factory);
        self
    }

    /// The same registration, claiming `tags` on top of whatever it claimed.
    pub fn tags(mut self, tags: impl IntoIterator<Item = CodecTag>) -> CodecInfo {
        self.tags.extend(tags);
        self
    }
}

/// The codecs a caller has registered, in registration order per id.
///
/// The replica builds one of these per AC-3 segment, registers that crate's
/// codecs into it and asks for the first decoder that claims the parameters.
#[derive(Clone, Default)]
pub struct CodecRegistry {
    by_id: HashMap<CodecId, Vec<CodecInfo>>,
}

impl CodecRegistry {
    /// An empty registry.
    pub fn new() -> CodecRegistry {
        CodecRegistry::default()
    }

    /// Add one codec.
    pub fn register(&mut self, info: CodecInfo) {
        self.by_id.entry(info.id.clone()).or_default().push(info);
    }

    /// True when something registered can decode `id`.
    pub fn has_decoder(&self, id: &CodecId) -> bool {
        self.by_id
            .get(id)
            .is_some_and(|infos| infos.iter().any(|i| i.decoder_factory.is_some()))
    }

    /// A decoder from the first registration that offers one.
    pub fn first_decoder(&self, params: &CodecParameters) -> Result<Box<dyn Decoder>> {
        let factory = self
            .by_id
            .get(&params.codec_id)
            .and_then(|infos| infos.iter().find_map(|i| i.decoder_factory))
            .ok_or_else(|| Error::CodecNotFound(params.codec_id.0.clone()))?;
        factory(params)
    }
}

/// What a codec crate is handed to register itself into.
///
/// The incumbent's context carries four sub-registries; this one carries the
/// codecs, because a container, source or filter crate registering into the
/// replica's build does not exist.
#[derive(Clone, Default)]
pub struct RuntimeContext {
    /// Decoder and encoder factories, by codec id.
    pub codecs: CodecRegistry,
}

impl RuntimeContext {
    /// An empty context.
    pub fn new() -> RuntimeContext {
        RuntimeContext::default()
    }
}

/// Declare a codec crate's registration entry point, as the family's crates
/// spell it: `oxideav_core::register!("h265", register);`.
///
/// The name is carried for documentation only, exactly as the incumbent does —
/// what the generated `__oxideav_entry` runs is `$func`.
#[macro_export]
macro_rules! register {
    ($name:literal, $func:path) => {
        /// This crate's registration entry point, generated by
        /// [`oxideav_core::register!`].
        #[doc(hidden)]
        pub fn __oxideav_entry(ctx: &mut $crate::RuntimeContext) {
            let _ = $name;
            $func(ctx);
        }
    };
}

/// A [`Decoder`] over one of the family's own, converting the frames on the way
/// out. Codec shims are two lines with this: build the `ec_core` decoder, wrap.
pub struct EcDecoder {
    id: CodecId,
    inner: Box<dyn ec_core::Decoder>,
}

impl EcDecoder {
    /// Wrap `inner`, which decodes the codec named `id`.
    pub fn new(id: CodecId, inner: Box<dyn ec_core::Decoder>) -> EcDecoder {
        EcDecoder { id, inner }
    }
}

impl Decoder for EcDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        Ok(self.inner.send_packet(packet)?)
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        Ok(self.inner.receive_frame()?.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoder that answers one frame per packet, for the registry test.
    struct Stub(CodecId);

    impl Decoder for Stub {
        fn codec_id(&self) -> &CodecId {
            &self.0
        }

        fn send_packet(&mut self, _packet: &Packet) -> Result<()> {
            Ok(())
        }

        fn receive_frame(&mut self) -> Result<Frame> {
            Err(Error::NeedMore)
        }
    }

    fn stub(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
        Ok(Box::new(Stub(params.codec_id.clone())))
    }

    #[test]
    fn the_registry_hands_back_what_was_registered_and_names_what_was_not() {
        let mut registry = CodecRegistry::new();
        registry.register(CodecInfo::new(CodecId::new("ac3")).decoder(stub));
        let params = CodecParameters::audio(CodecId::new("ac3"));
        let mut decoder = registry.first_decoder(&params).unwrap();
        assert_eq!(decoder.codec_id().as_str(), "ac3");
        assert!(matches!(decoder.receive_frame(), Err(Error::NeedMore)));
        assert!(registry.has_decoder(&CodecId::new("ac3")));

        let missing = CodecParameters::audio(CodecId::new("truehd"));
        assert!(matches!(
            registry.first_decoder(&missing),
            Err(Error::CodecNotFound(id)) if id == "truehd"
        ));
    }

    #[test]
    fn frames_and_errors_cross_the_boundary_intact() {
        let plane = ec_core::Plane::new(vec![1u8, 2, 3, 4], 4);
        let video =
            ec_core::VideoFrame::try_new(ec_core::PixelFormat::Rgba8, 1, 1, vec![plane]).unwrap();
        let Frame::Video(frame) = Frame::from(ec_core::Frame::Video(video)) else {
            panic!("a video frame stays a video frame");
        };
        assert_eq!(frame.planes[0].data, vec![1, 2, 3, 4]);
        assert_eq!(frame.planes[0].stride, 4);

        // The one error the replica matches on by name.
        assert!(matches!(
            Error::from(ec_core::Error::NeedMore),
            Error::NeedMore
        ));
        assert_eq!(
            Error::from(ec_core::Error::corrupt("torn")).to_string(),
            "invalid data: torn"
        );
    }

    #[test]
    fn a_packet_is_written_the_way_the_replica_writes_one() {
        let packet = Packet::new(0, TimeBase::new(1, 90_000), vec![0u8; 8]);
        assert_eq!(packet.data.len(), 8);
        assert_eq!(packet.time_base, TimeBase::new(1, 90_000));
    }
}

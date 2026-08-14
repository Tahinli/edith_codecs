//! Tracks, the reader trait, and seeking.

use crate::Result;
use crate::codecs::CodecParameters;
use crate::packet::Packet;
use crate::units::{Time, TimeBase, Timestamp};

/// What kind of stream a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackType {
    /// Sound.
    Audio,
    /// Pictures.
    Video,
    /// Timed text or bitmaps.
    Subtitle,
}

/// One track of a container.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    /// The container's own id for the track — a Matroska `TrackNumber`, which
    /// is what names one language of a dual-audio file.
    pub id: u32,
    /// Which kind of stream it is.
    pub track_type: TrackType,
    /// Base of this track's timestamps.
    pub time_base: Option<TimeBase>,
    /// Sample frames the track holds, when the container states one.
    pub num_frames: Option<u64>,
    /// First timestamp.
    pub start_ts: u64,
    /// Samples the decoder emits before the first audible one -- an MP3's LAME
    /// encoder delay, an Opus stream's pre-skip. **Not** dropped by the reader:
    /// [`num_frames`](Self::num_frames) counts them, so a caller that wants the
    /// audible stream skips them and takes `num_frames - delay` samples.
    pub delay: Option<u64>,
    /// Codec and setup data.
    pub codec_params: Option<CodecParameters>,
    /// ISO 639 language tag, when the container carries one.
    pub language: Option<String>,
}

/// Options a reader is built with; nothing here has any.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FormatOptions {
    /// Build a seek index even when the container carries none.
    pub enable_gapless: bool,
}

/// How exactly a seek must land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeekMode {
    /// The nearest random access point at or before the target, from which a
    /// decoder reaches the target exactly by decoding forward.
    #[default]
    Accurate,
    /// Whatever is cheapest.
    Coarse,
}

/// Where to seek to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SeekTo {
    /// An instant, on `track_id` or on the default track.
    Time {
        /// The instant.
        time: Time,
        /// Which track's timeline it is on.
        track_id: Option<u32>,
    },
    /// A timestamp in a track's own base.
    TimeStamp {
        /// The timestamp.
        ts: Timestamp,
        /// Which track it belongs to.
        track_id: u32,
    },
}

/// Where a seek actually landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekedTo {
    /// Track the landing is on.
    pub track_id: u32,
    /// What was asked for.
    pub required_ts: Timestamp,
    /// What was reached — at or before `required_ts` in [`SeekMode::Accurate`].
    pub actual_ts: Timestamp,
}

/// A container reader.
pub trait FormatReader: Send {
    /// Every track the container declares.
    fn tracks(&self) -> &[Track];

    /// The first track of `kind`.
    fn default_track(&self, kind: TrackType) -> Option<&Track>;

    /// Seek, answering where it landed.
    fn seek(&mut self, mode: SeekMode, to: SeekTo) -> Result<SeekedTo>;

    /// The next packet, or `Ok(None)` at the end of the stream.
    fn next_packet(&mut self) -> Result<Option<Packet>>;
}

/// Format hints from outside the bytes.
pub mod probe {
    /// What the caller knows that the content does not say: a file extension,
    /// a MIME type. Advisory — every reader in this family sniffs the content
    /// first and is free to disagree.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Hint {
        extension: Option<String>,
        mime_type: Option<String>,
    }

    impl Hint {
        /// A hint that says nothing.
        pub fn new() -> Hint {
            Hint::default()
        }

        /// Name the file extension, without its dot.
        pub fn with_extension(&mut self, extension: &str) -> &mut Hint {
            self.extension = Some(extension.to_string());
            self
        }

        /// Name the MIME type.
        pub fn mime_type(&mut self, mime: &str) -> &mut Hint {
            self.mime_type = Some(mime.to_string());
            self
        }

        /// The extension, if one was given.
        pub fn extension(&self) -> Option<&str> {
            self.extension.as_deref()
        }
    }
}

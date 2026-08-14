//! Text subtitles: SubRip and WebVTT into one cue IR ([`ir`]).
//!
//! Both formats are read the same way and for the same reason: what a timeline
//! wants is *when* a line is on screen and *what it says*, and the markup
//! around that ranges from `<i>` to a dozen dialects of override tag. So a
//! parse answers with a [`ir::SubtitleTrack`] — cues of time ranges and
//! [`ir::Segment`] runs — and a caller that draws nothing yet takes
//! [`ir::plain_text`] and drops the rest.
//!
//! Tolerance is the point. Subtitle files are hand-edited, machine-translated,
//! re-encoded and truncated; a parser that refuses one is a parser that refuses
//! the file the user actually has. BOMs (UTF-8 and UTF-16), CRLF and lone CR,
//! invalid UTF-8, missing or wrong indices, dotted fractions, overlapping and
//! backwards cues all parse. A block this cannot read is skipped and counted
//! ([`ir::SubtitleTrack::skipped`]); only a document with no timing line in it
//! at all is an error, because that is a document of another kind.
//!
//! ASS/SSA is the same IR from `ec-ass`, and PGS bitmaps from `ec-pgs`.

#![forbid(unsafe_code)]

pub mod ir;
mod markup;
pub mod srt;
pub mod webvtt;

pub use ir::{
    CuePosition, Segment, SourceFormat, SubtitleCue, SubtitleStyle, SubtitleTrack, TextAlign,
    plain_text,
};
/// The byte-level tolerances every subtitle format needs, shared with `ec-ass`:
/// BOM and line-ending normalisation, and the clock every one of them writes.
pub use markup::{decode, parse_clock};

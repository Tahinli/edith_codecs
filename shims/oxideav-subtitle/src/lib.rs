//! `oxideav-subtitle` as edith consumes it, over [`ec_subs`].
//!
//! Three items and their types: `ir::plain_text`, `ir::SubtitleTrack`,
//! `srt::parse` and `webvtt::parse`. The IR is `ec_subs`'s own — the field
//! names were chosen to match, so this is a re-export and not a conversion —
//! and the only adapter is the error type, because the replica writes the parse
//! functions as `fn(&[u8]) -> oxideav_core::Result<SubtitleTrack>` and takes
//! their address.

#![forbid(unsafe_code)]

/// The cue IR, which is [`ec_subs::ir`] under its incumbent name.
pub mod ir {
    pub use ec_subs::ir::{
        CuePosition, Segment, SourceFormat, SubtitleCue, SubtitleStyle, SubtitleTrack, TextAlign,
        plain_text,
    };
}

pub use ir::{SourceFormat, SubtitleTrack};

/// SubRip.
pub mod srt {
    use oxideav_core::Result;

    use crate::ir::SubtitleTrack;

    /// Parse a SubRip document.
    pub fn parse(bytes: &[u8]) -> Result<SubtitleTrack> {
        Ok(ec_subs::srt::parse(bytes)?)
    }
}

/// WebVTT.
pub mod webvtt {
    use oxideav_core::Result;

    use crate::ir::SubtitleTrack;

    /// Parse a WebVTT document.
    pub fn parse(bytes: &[u8]) -> Result<SubtitleTrack> {
        Ok(ec_subs::webvtt::parse(bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The replica takes these as function pointers of one type and calls
    /// `plain_text` on whatever comes back; that is the whole contract.
    #[test]
    fn the_parsers_are_interchangeable_function_pointers() {
        type Parse = fn(&[u8]) -> oxideav_core::Result<ir::SubtitleTrack>;
        for parse in [srt::parse as Parse, webvtt::parse as Parse] {
            let track = parse(b"1\n00:00:01,000 --> 00:00:02,000\n<i>hi</i>\n").unwrap();
            assert_eq!(ir::plain_text(&track.cues[0].segments), "hi");
            assert_eq!(track.cues[0].start_us, 1_000_000);
        }
        // A file of the wrong kind is an error the replica can print.
        let error = srt::parse(b"prose, not subtitles").unwrap_err();
        assert!(!error.to_string().is_empty());
    }
}

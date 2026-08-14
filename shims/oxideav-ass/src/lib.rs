//! `oxideav-ass` as edith consumes it, over [`ec_ass`].
//!
//! One item: `parse`, answering the same [`SubtitleTrack`] the SubRip and
//! WebVTT parsers do — the replica hands all three around as one function
//! pointer type and reads `cues`, `segments` and `plain_text` off the result.

#![forbid(unsafe_code)]

use oxideav_core::Result;

pub use ec_subs::ir::{
    CuePosition, Segment, SourceFormat, SubtitleCue, SubtitleStyle, SubtitleTrack, TextAlign,
};

/// Parse an ASS or SSA script.
pub fn parse(bytes: &[u8]) -> Result<SubtitleTrack> {
    Ok(ec_ass::parse(bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_parses_through_the_shim() {
        type Parse = fn(&[u8]) -> Result<SubtitleTrack>;
        let parse: Parse = parse;
        let doc = "[Events]\n\
            Format: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text\n\
            Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\i1}hi{\\i0}\\Nthere\n";
        let track = parse(doc.as_bytes()).unwrap();
        assert_eq!(track.cues.len(), 1);
        assert_eq!(track.cues[0].start_us, 1_000_000);
        assert_eq!(ec_subs::plain_text(&track.cues[0].segments), "hi\nthere");
        assert!(!parse(b"not a script").unwrap_err().to_string().is_empty());
    }
}

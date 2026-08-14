//! SubRip (`.srt`).
//!
//! The format is a list of blocks separated by blank lines: an index, a timing
//! line, then the text. In the wild the index is optional, wrong, or repeated;
//! the fraction is a comma or a dot; the timing line trails coordinates
//! (`X1:0 X2:640 …`) that nothing reads any more. So the *timing line* is what
//! identifies a block here — a block without one is skipped and counted rather
//! than taken as text of the block before it.

use ec_core::{Error, Result};

use crate::ir::{SourceFormat, SubtitleCue, SubtitleTrack};
use crate::markup::{decode, parse_clock, parse_inline};

/// Parse a SubRip document.
///
/// `Err` only when the bytes are not this format at all — text with no timing
/// line anywhere in it. A file with some torn blocks parses to the blocks that
/// are whole, with [`SubtitleTrack::skipped`] counting the rest.
pub fn parse(bytes: &[u8]) -> Result<SubtitleTrack> {
    let text = decode(bytes);
    let mut track = SubtitleTrack::new().with_source(SourceFormat::Srt);
    for block in blocks(&text) {
        // The timing line is the anchor: everything before it is an index (or
        // rubbish), everything after it is the cue's text.
        let Some((at, (start_us, end_us))) = block
            .iter()
            .enumerate()
            .find_map(|(i, line)| timing(line).map(|t| (i, t)))
        else {
            track.skipped += 1;
            continue;
        };
        let body = block[at + 1..].join("\n");
        track.cues.push(SubtitleCue {
            start_us,
            // A file that ends a cue before it starts says nothing about how
            // long it is up; the cue is kept, with no duration rather than a
            // negative one.
            end_us: end_us.max(start_us),
            style_ref: None,
            positioning: None,
            segments: parse_inline(body.trim_end(), false),
        });
    }
    if track.cues.is_empty() && text.split_whitespace().next().is_some() {
        return Err(Error::corrupt(
            "no SubRip timing line in the whole document",
        ));
    }
    Ok(track)
}

/// Blocks, split on blank lines, blank ones dropped.
fn blocks(text: &str) -> impl Iterator<Item = Vec<&str>> {
    let mut out: Vec<Vec<&str>> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out.into_iter()
}

/// `00:00:01,000 --> 00:00:02,000` with anything after it ignored.
pub(crate) fn timing(line: &str) -> Option<(i64, i64)> {
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next().unwrap_or_default();
    Some((parse_clock(start)?, parse_clock(end)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::plain_text;

    #[test]
    fn an_ordinary_file_parses() {
        let doc = "1\n00:00:01,000 --> 00:00:02,500\nfirst line\nsecond line\n\n\
                   2\n00:00:03,000 --> 00:00:04,000\n<i>tilted</i>\n";
        let track = parse(doc.as_bytes()).unwrap();
        assert_eq!(track.source, Some(SourceFormat::Srt));
        assert_eq!(track.cues.len(), 2);
        assert_eq!(track.cues[0].start_us, 1_000_000);
        assert_eq!(track.cues[0].end_us, 2_500_000);
        assert_eq!(
            plain_text(&track.cues[0].segments),
            "first line\nsecond line"
        );
        assert_eq!(plain_text(&track.cues[1].segments), "tilted");
        assert_eq!(track.skipped, 0);
    }

    #[test]
    fn the_tolerances_a_real_file_needs() {
        // BOM, CRLF, no indices, dotted fraction, trailing coordinates, an
        // overlapping pair, and a block that is text with no timing at all.
        let doc = b"\xef\xbb\xbf00:00:01.000 --> 00:00:04,000 X1:0 X2:640\r\none\r\n\r\n\
                    junk with no timing\r\n\r\n\
                    00:00:02,000 --> 00:00:03,000\r\ntwo\r\n";
        let track = parse(doc).unwrap();
        assert_eq!(track.cues.len(), 2);
        assert_eq!(track.skipped, 1);
        assert_eq!(track.cues[0].end_us, 4_000_000);
        // Overlapping cues stay in file order, both of them.
        assert_eq!(track.cues[1].start_us, 2_000_000);
        assert_eq!(plain_text(&track.cues[1].segments), "two");
    }

    #[test]
    fn a_backwards_pair_is_clamped_not_negative() {
        let doc = "1\n00:00:05,000 --> 00:00:01,000\nback\n";
        let cue = &parse(doc.as_bytes()).unwrap().cues[0];
        assert_eq!((cue.start_us, cue.end_us), (5_000_000, 5_000_000));
    }

    #[test]
    fn text_that_is_not_a_subtitle_file_is_refused_but_empty_is_not() {
        assert!(parse(b"this is a text file\nwith no timings").is_err());
        assert_eq!(parse(b"").unwrap().cues.len(), 0);
        assert_eq!(parse(b"\n\n  \n").unwrap().cues.len(), 0);
    }
}

//! WebVTT (`.vtt`).
//!
//! SubRip's shape with a `WEBVTT` header, optional cue identifiers, cue
//! settings after the timing line, and two tags of its own (`<v Speaker>`,
//! `<c.class>`) plus inline timestamps. `NOTE`, `STYLE` and `REGION` blocks
//! are read past: they say nothing about *when* anything is on screen, which
//! is what this IR carries.

use ec_core::{Error, Result};

use crate::ir::{CuePosition, SourceFormat, SubtitleCue, SubtitleTrack, TextAlign};
use crate::markup::{decode, parse_inline};
use crate::srt::timing;

/// Parse a WebVTT document.
///
/// The `WEBVTT` signature is *expected*, not required — a file missing it but
/// full of cues is a file of cues. `Err` is for text with no timing line
/// anywhere, exactly as in [`crate::srt::parse`].
pub fn parse(bytes: &[u8]) -> Result<SubtitleTrack> {
    let text = decode(bytes);
    let mut track = SubtitleTrack::new().with_source(SourceFormat::WebVtt);
    let mut blocks = text.split("\n\n").peekable();
    if let Some(first) = blocks.peek()
        && let Some(header) = first.trim_start().strip_prefix("WEBVTT")
    {
        let header = header.trim();
        if !header.is_empty() {
            track
                .metadata
                .push(("header".to_owned(), header.to_owned()));
        }
        track.extradata = first.trim_start().as_bytes().to_vec();
        blocks.next();
    }
    for block in blocks {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some(first) = lines.first() else { continue };
        // Blocks that are not cues, by their first word.
        let keyword = first.split_whitespace().next().unwrap_or_default();
        if matches!(keyword, "NOTE" | "STYLE" | "REGION") {
            continue;
        }
        let Some((at, (start_us, end_us))) = lines
            .iter()
            .enumerate()
            .find_map(|(i, line)| timing(line).map(|t| (i, t)))
        else {
            track.skipped += 1;
            continue;
        };
        // Anything before the timing line is the cue identifier, which names
        // the cue for styling and says nothing about its timing or its text.
        let body = lines[at + 1..].join("\n");
        track.cues.push(SubtitleCue {
            start_us,
            end_us: end_us.max(start_us),
            style_ref: None,
            positioning: settings(lines[at]),
            segments: parse_inline(body.trim_end(), true),
        });
    }
    if track.cues.is_empty() && text.split_whitespace().next().is_some() {
        return Err(Error::corrupt(
            "no WebVTT timing line in the whole document",
        ));
    }
    Ok(track)
}

/// The `align:`, `position:`, `line:` and `size:` settings after a timing line.
/// `None` when the line carries none of them.
fn settings(line: &str) -> Option<CuePosition> {
    let after = line.split_once("-->")?.1;
    // The end timestamp first, then the settings.
    let mut position = CuePosition::default();
    let mut any = false;
    for setting in after.split_whitespace().skip(1) {
        let Some((key, value)) = setting.split_once(':') else {
            continue;
        };
        // `position:10%,line-left` — the alignment suffix is not the number.
        let number = || {
            value
                .split(',')
                .next()
                .unwrap_or_default()
                .trim_end_matches('%')
                .parse::<f32>()
                .ok()
        };
        match key {
            "align" => {
                position.align = match value {
                    "start" => TextAlign::Start,
                    "center" | "middle" => TextAlign::Center,
                    "end" => TextAlign::End,
                    "left" => TextAlign::Left,
                    "right" => TextAlign::Right,
                    _ => continue,
                };
                any = true;
            }
            "position" => {
                position.x = number();
                any |= position.x.is_some();
            }
            // A `line` without a `%` is a line *number*, not a percentage;
            // it is carried as given, which is what the renderer will need.
            "line" => {
                position.y = number();
                any |= position.y.is_some();
            }
            "size" => {
                position.size = number();
                any |= position.size.is_some();
            }
            _ => {}
        }
    }
    any.then_some(position)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Segment, plain_text};

    #[test]
    fn header_identifiers_notes_and_settings() {
        let doc = "WEBVTT - a title\n\n\
                   NOTE this block says nothing\n\n\
                   cue-1\n00:00:01.000 --> 00:00:02.000 align:center position:40% line:90%\n\
                   <v Ann>hello</v>\n\n\
                   00:01:00.000 --> 00:01:02.000\nplain\n";
        let track = parse(doc.as_bytes()).unwrap();
        assert_eq!(track.source, Some(SourceFormat::WebVtt));
        assert_eq!(
            track.metadata,
            vec![("header".to_owned(), "- a title".to_owned())]
        );
        assert_eq!(track.cues.len(), 2);
        let first = &track.cues[0];
        assert_eq!((first.start_us, first.end_us), (1_000_000, 2_000_000));
        let position = first.positioning.unwrap();
        assert_eq!(position.align, TextAlign::Center);
        assert_eq!((position.x, position.y), (Some(40.0), Some(90.0)));
        assert_eq!(plain_text(&first.segments), "hello");
        assert!(matches!(first.segments[0], Segment::Voice { .. }));
        assert_eq!(track.cues[1].positioning, None);
        assert_eq!(track.cues[1].start_us, 60_000_000);
    }

    #[test]
    fn inline_timestamps_and_classes_are_runs_not_words() {
        let doc = "WEBVTT\n\n00:00:00.000 --> 00:00:09.000\n\
                   <c.loud>shout</c><00:00:03.000>then quiet\n";
        let cue = &parse(doc.as_bytes()).unwrap().cues[0];
        assert_eq!(plain_text(&cue.segments), "shoutthen quiet");
        assert!(cue.segments.iter().any(|s| matches!(
            s,
            Segment::Timestamp {
                offset_us: 3_000_000
            }
        )));
    }

    #[test]
    fn a_file_without_the_signature_still_parses_and_prose_does_not() {
        let track = parse(b"00:00:01.000 --> 00:00:02.000\nhi\n").unwrap();
        assert_eq!(track.cues.len(), 1);
        assert!(parse(b"WEBVTT\n\nNOTE just a note\n\nnot a cue block").is_err());
    }
}

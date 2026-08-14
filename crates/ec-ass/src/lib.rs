//! Advanced SubStation Alpha, and the SSA v4 it grew out of.
//!
//! An `.ass` is an INI: `[Script Info]` keys, a `[V4+ Styles]` table and an
//! `[Events]` table, each table declaring its own column order on a `Format:`
//! line. That declaration is why this parses the header rather than assuming
//! positions — Aegisub's order is the usual one, but the file gets to say.
//!
//! The cue text carries override tags in braces: `{\i1}` turns italics on until
//! `{\i0}`, `{\an8}` puts the line at the top, `{\pos(x,y)}` puts it exactly,
//! `{\k30}` is a karaoke beat, and `{\p1}` switches the *text* to vector
//! drawing commands until `{\p0}`. Those become [`ec_subs::Segment`] runs, so
//! a caller that wants words ([`ec_subs::plain_text`]) gets words and a caller
//! that will one day draw them has the tree.
//!
//! What the format states and this drops on the floor: transforms, animation,
//! clipping and every other override that only a renderer can honour. They are
//! kept verbatim as [`ec_subs::Segment::Raw`] rather than deleted.

#![forbid(unsafe_code)]

use ec_core::{Error, Result};
use ec_subs::ir::{
    CuePosition, Segment, SourceFormat, SubtitleCue, SubtitleStyle, SubtitleTrack, TextAlign,
};
use ec_subs::{decode, parse_clock};

/// Parse an ASS or SSA document.
///
/// `Err` only when there is no `Dialogue:` line and no `[Events]` section in
/// the whole document — that is not this format. Torn rows are skipped and
/// counted in [`SubtitleTrack::skipped`], because one bad line in a
/// twelve-hundred-line script is not a reason to lose the other eleven hundred.
pub fn parse(bytes: &[u8]) -> Result<SubtitleTrack> {
    let text = decode(bytes);
    let mut track = SubtitleTrack::new().with_source(SourceFormat::AssOrSsa);
    let mut section = Section::None;
    let mut style_format: Vec<String> = Vec::new();
    let mut event_format: Vec<String> = Vec::new();
    let mut header_end = 0;
    let mut in_events = false;
    let mut saw_events = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        // Everything up to the first event row is the script header — what a
        // Matroska track carries as `CodecPrivate`.
        if trimmed.starts_with("Dialogue:") || trimmed.starts_with("Comment:") {
            in_events = true;
        } else if !in_events {
            header_end += line.len();
        }
        if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('!') {
            continue;
        }
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = Section::of(name);
            saw_events |= matches!(section, Section::Events);
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match (section, key.trim()) {
            (Section::ScriptInfo, key) => {
                track.metadata.push((snake_case(key), value.to_owned()));
            }
            (Section::Styles { .. }, "Format") => style_format = fields(value),
            (Section::Styles { v4 }, "Style") => match style(&style_format, value, v4) {
                Some(style) => track.styles.push(style),
                None => track.skipped += 1,
            },
            (Section::Events, "Format") => event_format = fields(value),
            (Section::Events, "Dialogue") => {
                let wrap_style = track
                    .metadata
                    .iter()
                    .find(|(k, _)| k == "wrap_style")
                    .and_then(|(_, v)| v.parse::<u8>().ok())
                    .unwrap_or(0);
                match dialogue(&event_format, value, wrap_style) {
                    Some(cue) => track.cues.push(cue),
                    None => track.skipped += 1,
                }
            }
            // `Comment:` events are the script's own scratch lines: real rows
            // of the events table, never on screen.
            _ => {}
        }
    }
    if track.cues.is_empty() && !saw_events {
        return Err(Error::corrupt(
            "no [Events] section: not an ASS or SSA script",
        ));
    }
    track.extradata = text.as_bytes()[..header_end.min(text.len())].to_vec();
    Ok(track)
}

#[derive(Clone, Copy)]
enum Section {
    None,
    ScriptInfo,
    /// `v4` marks the SSA-era table, whose alignment codes differ.
    Styles {
        v4: bool,
    },
    Events,
}

impl Section {
    fn of(name: &str) -> Section {
        let lower = name.trim().to_ascii_lowercase();
        match lower.as_str() {
            "script info" => Section::ScriptInfo,
            "events" => Section::Events,
            "v4 styles" => Section::Styles { v4: true },
            _ if lower.ends_with("styles") => Section::Styles { v4: false },
            _ => Section::None,
        }
    }
}

/// `PlayResX` → `play_res_x`, `Original Script` → `original_script`: metadata
/// keys are one shape whichever way the script wrote them.
fn snake_case(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    let mut previous = ' ';
    for c in key.trim().chars() {
        match c {
            ' ' | '-' => out.push('_'),
            c => {
                if c.is_ascii_uppercase()
                    && (previous.is_ascii_lowercase() || previous.is_ascii_digit())
                {
                    out.push('_');
                }
                out.push(c.to_ascii_lowercase());
            }
        }
        previous = c;
    }
    out
}

fn fields(value: &str) -> Vec<String> {
    value.split(',').map(|f| f.trim().to_owned()).collect()
}

/// Column `name` of a row, by the order the table's `Format:` line declared.
fn column<'a>(format: &[String], values: &'a [&'a str], name: &str) -> Option<&'a str> {
    let at = format.iter().position(|f| f.eq_ignore_ascii_case(name))?;
    values.get(at).map(|v| v.trim())
}

fn style(format: &[String], row: &str, v4: bool) -> Option<SubtitleStyle> {
    // A table with no `Format:` line at all uses the order every tool writes.
    const DEFAULT: &str = "Name,Fontname,Fontsize,PrimaryColour,SecondaryColour,OutlineColour,\
        BackColour,Bold,Italic,Underline,StrikeOut,ScaleX,ScaleY,Spacing,Angle,BorderStyle,\
        Outline,Shadow,Alignment,MarginL,MarginR,MarginV,Encoding";
    let fallback = fields(DEFAULT);
    let format = if format.is_empty() { &fallback } else { format };
    let values: Vec<&str> = row.split(',').collect();
    let name = column(format, &values, "Name")?;
    let number = |key: &str| column(format, &values, key).and_then(|v| v.parse::<f32>().ok());
    let margin = |key: &str| column(format, &values, key).and_then(|v| v.parse::<i32>().ok());
    let flag = |key: &str| column(format, &values, key).is_some_and(|v| v.trim() != "0");
    Some(SubtitleStyle {
        name: name.to_owned(),
        font_family: column(format, &values, "Fontname").map(str::to_owned),
        font_size: number("Fontsize"),
        primary_color: column(format, &values, "PrimaryColour").and_then(parse_color),
        outline_color: column(format, &values, "OutlineColour")
            .or_else(|| column(format, &values, "TertiaryColour"))
            .and_then(parse_color),
        back_color: column(format, &values, "BackColour").and_then(parse_color),
        bold: flag("Bold"),
        italic: flag("Italic"),
        underline: flag("Underline"),
        strike: flag("StrikeOut"),
        align: column(format, &values, "Alignment")
            .and_then(|v| v.parse().ok())
            .map(|code| alignment(code, v4))
            .unwrap_or_default(),
        margin_l: margin("MarginL"),
        margin_r: margin("MarginR"),
        margin_v: margin("MarginV"),
        outline: number("Outline"),
        shadow: number("Shadow"),
    })
}

/// ASS alignment is the numeric keypad (`\an`): 1/4/7 left, 2/5/8 centre,
/// 3/6/9 right. SSA v4 used a different code with the same three columns in
/// its low two bits.
fn alignment(code: i32, v4: bool) -> TextAlign {
    let column = match v4 {
        true => code & 0b11,
        false => (code - 1) % 3 + 1,
    };
    match column {
        1 => TextAlign::Left,
        2 => TextAlign::Center,
        3 => TextAlign::Right,
        _ => TextAlign::default(),
    }
}

/// `&HAABBGGRR&`: BGR with a *transparency* byte, so 0 is opaque.
fn parse_color(value: &str) -> Option<(u8, u8, u8, u8)> {
    let digits: String = value
        .trim()
        .trim_start_matches(['&', 'H', 'h'])
        .trim_end_matches('&')
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect();
    let packed = u32::from_str_radix(&digits, 16).ok()?;
    Some((
        (packed & 0xFF) as u8,
        ((packed >> 8) & 0xFF) as u8,
        ((packed >> 16) & 0xFF) as u8,
        255 - ((packed >> 24) & 0xFF) as u8,
    ))
}

fn dialogue(format: &[String], row: &str, wrap_style: u8) -> Option<SubtitleCue> {
    const DEFAULT: &str = "Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text";
    let fallback = fields(DEFAULT);
    let format = if format.is_empty() { &fallback } else { format };
    // `Text` is last and holds commas of its own, so the row is split into
    // exactly as many fields as the table declares and no further.
    let values: Vec<&str> = row.splitn(format.len(), ',').collect();
    let start_us = parse_clock(column(format, &values, "Start")?)?;
    let end_us = parse_clock(column(format, &values, "End")?)?;
    let text = column(format, &values, "Text").unwrap_or_default();
    let (segments, positioning) = body(text, wrap_style);
    Some(SubtitleCue {
        start_us,
        end_us: end_us.max(start_us),
        style_ref: column(format, &values, "Style")
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        positioning,
        segments,
    })
}

/// The state an override block leaves behind for the runs after it.
#[derive(Clone, Default)]
struct Overrides {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    color: Option<(u8, u8, u8)>,
    family: Option<String>,
    size: Option<f32>,
    karaoke: Option<u32>,
    /// `\p1` and up: the text is a drawing, not words.
    drawing: bool,
}

/// One `Text` field into runs, plus whatever positioning its overrides state.
fn body(text: &str, wrap_style: u8) -> (Vec<Segment>, Option<CuePosition>) {
    let mut out: Vec<Segment> = Vec::new();
    let mut state = Overrides::default();
    let mut position = CuePosition::default();
    let mut positioned = false;
    let mut run = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('{') {
        let (before, after) = rest.split_at(at);
        push_text(&mut out, &mut run, before, &state, wrap_style);
        let Some(end) = after.find('}') else {
            // An unbalanced brace: the rest of the line is text, braces and all.
            run.push_str(after);
            rest = "";
            break;
        };
        let block = &after[1..end];
        rest = &after[end + 1..];
        flush(&mut out, &mut run, &state);
        if let Some(unknown) = overrides(block, &mut state, &mut position, &mut positioned) {
            out.push(Segment::Raw(unknown));
        }
    }
    push_text(&mut out, &mut run, rest, &state, wrap_style);
    flush(&mut out, &mut run, &state);
    (out, positioned.then_some(position))
}

/// Apply one brace block's overrides. Returns the overrides this does not
/// model, verbatim, for a [`Segment::Raw`].
fn overrides(
    block: &str,
    state: &mut Overrides,
    position: &mut CuePosition,
    positioned: &mut bool,
) -> Option<String> {
    let mut unknown = String::new();
    for tag in block.split('\\').skip(1) {
        let tag = tag.trim_end();
        if tag.is_empty() {
            continue;
        }
        let Some((name, arg)) = split_tag(tag) else {
            unknown.push_str(&format!("\\{tag}"));
            continue;
        };
        let flag = || !arg.trim().trim_start_matches('0').is_empty();
        match name {
            "b" => state.bold = flag(),
            "i" => state.italic = flag(),
            "u" => state.underline = flag(),
            "s" => state.strike = flag(),
            "c" | "1c" => state.color = parse_color(arg).map(|(r, g, b, _)| (r, g, b)),
            "fn" => state.family = Some(arg.trim().to_owned()).filter(|f| !f.is_empty()),
            "fs" => state.size = arg.trim().parse().ok(),
            "k" | "K" | "kf" | "ko" => state.karaoke = arg.trim().parse().ok(),
            "p" => state.drawing = flag(),
            "an" => {
                if let Ok(code) = arg.trim().parse::<i32>() {
                    position.align = alignment(code, false);
                    *positioned = true;
                }
            }
            "a" => {
                if let Ok(code) = arg.trim().parse::<i32>() {
                    position.align = alignment(code, true);
                    *positioned = true;
                }
            }
            "pos" | "move" => {
                // `\move(x1,y1,x2,y2,…)` starts where `\pos(x,y)` would put it.
                let mut parts = arg
                    .trim_matches(['(', ')'])
                    .split(',')
                    .filter_map(|n| n.trim().parse().ok());
                position.x = parts.next();
                position.y = parts.next();
                *positioned = true;
                if name == "move" {
                    unknown.push_str(&format!("\\{tag}"));
                }
            }
            _ => unknown.push_str(&format!("\\{tag}")),
        }
    }
    (!unknown.is_empty()).then(|| format!("{{{unknown}}}"))
}

/// An override into its name and its argument.
///
/// The names overlap — `\b` is bold and `\blur` is a blur — so a one-letter
/// name is only that name when what follows it could be its argument: a
/// number, a `&H` colour, a bracketed list, or nothing at all. Everything else
/// falls through to [`Segment::Raw`] rather than being half-read.
fn split_tag(tag: &str) -> Option<(&str, &str)> {
    const NAMES: [&str; 16] = [
        "move", "pos", "fn", "fs", "kf", "ko", "an", "1c", "c", "b", "i", "u", "s", "k", "p", "a",
    ];
    NAMES.iter().find_map(|name| {
        let arg = tag.strip_prefix(name)?;
        // A font name is letters, so `\fn` takes whatever follows it.
        let plausible = *name == "fn"
            || arg.is_empty()
            || arg.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '&' | '(' | '.'));
        plausible.then_some((*name, arg))
    })
}

/// Plain text between override blocks, with the three escapes ASS gives it.
fn push_text(
    out: &mut Vec<Segment>,
    run: &mut String,
    text: &str,
    state: &Overrides,
    wrap_style: u8,
) {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            run.push(c);
            continue;
        }
        match chars.next() {
            // `\N` always breaks; `\n` breaks only where the script asked for
            // no automatic wrapping (`WrapStyle: 2`), and is a space otherwise,
            // which is what every renderer does with it.
            Some('N') => {
                flush(out, run, state);
                out.push(Segment::LineBreak);
            }
            Some('n') if wrap_style == 2 => {
                flush(out, run, state);
                out.push(Segment::LineBreak);
            }
            Some('n') => run.push(' '),
            Some('h') => run.push('\u{a0}'),
            Some(other) => {
                run.push('\\');
                run.push(other);
            }
            None => run.push('\\'),
        }
    }
}

/// Close the pending run, wrapping it in whatever the overrides asked for.
fn flush(out: &mut Vec<Segment>, run: &mut String, state: &Overrides) {
    if run.is_empty() {
        return;
    }
    let text = std::mem::take(run);
    // A drawing is shapes, not words: kept whole so a renderer can have it and
    // `plain_text` says nothing.
    if state.drawing {
        out.push(Segment::Raw(text));
        return;
    }
    let mut segment = vec![Segment::Text(text)];
    if let Some(cs) = state.karaoke {
        segment = vec![Segment::Karaoke {
            cs,
            children: segment,
        }];
    }
    if state.family.is_some() || state.size.is_some() {
        segment = vec![Segment::Font {
            family: state.family.clone(),
            size: state.size,
            children: segment,
        }];
    }
    if let Some(rgb) = state.color {
        segment = vec![Segment::Color {
            rgb,
            children: segment,
        }];
    }
    for (on, wrap) in [
        (state.strike, Segment::Strike as fn(Vec<Segment>) -> Segment),
        (state.underline, Segment::Underline as fn(_) -> _),
        (state.italic, Segment::Italic as fn(_) -> _),
        (state.bold, Segment::Bold as fn(_) -> _),
    ] {
        if on {
            segment = vec![wrap(segment)];
        }
    }
    out.extend(segment);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_subs::plain_text;

    const SCRIPT: &str = "[Script Info]\n\
        Title: A script\n\
        WrapStyle: 0\n\
        PlayResX: 1920\n\
        \n\
        [V4+ Styles]\n\
        Format: Name, Fontname, Fontsize, PrimaryColour, OutlineColour, BackColour, Bold, Italic, \
        Underline, StrikeOut, Alignment, MarginL, MarginR, MarginV, Outline, Shadow, Encoding\n\
        Style: Default,Arial,48,&H00FFFFFF,&H00000000,&H80000000,-1,0,0,0,2,10,10,20,2,1,1\n\
        \n\
        [Events]\n\
        Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
        Dialogue: 0,0:00:01.00,0:00:03.50,Default,,0,0,0,,{\\i1}tilted{\\i0} plain\\Nsecond, with comma\n\
        Comment: 0,0:00:04.00,0:00:05.00,Default,,0,0,0,,not on screen\n\
        Dialogue: 0,0:00:06.00,0:00:07.00,Default,,0,0,0,,{\\an8\\pos(960,100)}up top\n";

    #[test]
    fn a_script_parses_header_styles_and_events() {
        let track = parse(SCRIPT.as_bytes()).unwrap();
        assert_eq!(track.source, Some(SourceFormat::AssOrSsa));
        assert_eq!(
            track.metadata.iter().find(|(k, _)| k == "title").unwrap().1,
            "A script"
        );
        let style = track.style("Default").unwrap();
        assert_eq!(style.font_family.as_deref(), Some("Arial"));
        assert_eq!(style.font_size, Some(48.0));
        assert!(style.bold && !style.italic);
        assert_eq!(style.primary_color, Some((255, 255, 255, 255)));
        // `&H80000000` is half transparent, and BGR order means blue is zero.
        assert_eq!(style.back_color, Some((0, 0, 0, 127)));
        assert_eq!(style.align, TextAlign::Center);

        // The `Comment:` row is not a cue.
        assert_eq!(track.cues.len(), 2);
        let cue = &track.cues[0];
        assert_eq!((cue.start_us, cue.end_us), (1_000_000, 3_500_000));
        assert_eq!(cue.style_ref.as_deref(), Some("Default"));
        // The text field keeps its own commas.
        assert_eq!(
            plain_text(&cue.segments),
            "tilted plain\nsecond, with comma"
        );
        assert!(matches!(cue.segments[0], Segment::Italic(_)));
        assert_eq!(track.skipped, 0);
        // The header is what a Matroska `CodecPrivate` carries.
        assert!(track.extradata.starts_with(b"[Script Info]"));
        assert!(!track.extradata.windows(9).any(|w| w == b"Dialogue:"));
    }

    #[test]
    fn positioning_overrides_reach_the_cue() {
        let track = parse(SCRIPT.as_bytes()).unwrap();
        let position = track.cues[1].positioning.unwrap();
        assert_eq!(position.align, TextAlign::Center);
        assert_eq!((position.x, position.y), (Some(960.0), Some(100.0)));
        assert_eq!(plain_text(&track.cues[1].segments), "up top");
    }

    #[test]
    fn karaoke_drawings_and_unknown_tags() {
        let doc = "[Events]\nFormat: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text\n\
            Dialogue: 0,0:00:00.00,0:00:01.00,,,0,0,0,,{\\k30}ka{\\k25}ra\n\
            Dialogue: 0,0:00:01.00,0:00:02.00,,,0,0,0,,{\\p1}m 0 0 l 10 10{\\p0}words\n\
            Dialogue: 0,0:00:02.00,0:00:03.00,,,0,0,0,,{\\t(0,500,\\fscx120)}moving\n";
        let track = parse(doc.as_bytes()).unwrap();
        assert_eq!(plain_text(&track.cues[0].segments), "kara");
        assert!(matches!(
            track.cues[0].segments[0],
            Segment::Karaoke { cs: 30, .. }
        ));
        // Drawing commands are not words.
        assert_eq!(plain_text(&track.cues[1].segments), "words");
        // An override this does not model survives as itself.
        assert_eq!(plain_text(&track.cues[2].segments), "moving");
        assert!(
            track.cues[2]
                .segments
                .iter()
                .any(|s| matches!(s, Segment::Raw(r) if r.contains("fscx120")))
        );
    }

    #[test]
    fn tolerances_and_the_refusal() {
        // No `Format:` line, CRLF, a torn row, and `\h` / `\n`.
        let doc = b"\xef\xbb\xbf[Events]\r\n\
            Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,a\\hb\\nc\r\n\
            Dialogue: not a row\r\n";
        let track = parse(doc).unwrap();
        assert_eq!(track.cues.len(), 1);
        assert_eq!(track.skipped, 1);
        assert_eq!(plain_text(&track.cues[0].segments), "a\u{a0}b c");
        // `WrapStyle: 2` makes `\n` a break instead.
        let wrapped = format!(
            "[Script Info]\nWrapStyle: 2\n{}",
            String::from_utf8_lossy(&doc[3..])
        );
        let track = parse(wrapped.as_bytes()).unwrap();
        assert_eq!(plain_text(&track.cues[0].segments), "a\u{a0}b\nc");

        assert!(parse(b"just some prose").is_err());
    }
}

//! The angle-bracket markup SubRip and WebVTT share, plus the byte-level
//! tolerances every text format here needs.
//!
//! Both formats carry a subset of HTML — `<i>`, `<b>`, `<u>`, `<s>`, `<font>`,
//! and WebVTT's `<v Speaker>`, `<c.class>` and inline timestamps — written by a
//! thousand tools with a thousand degrees of care. So the rule is that nothing
//! here fails: an unclosed tag closes at the end of the cue, a stray `</i>`
//! with nothing open is dropped, and a `<` that begins no tag at all is the
//! character it looks like.

use crate::ir::Segment;

/// Bytes to text, the way a subtitle file has to be read: a UTF-8 or UTF-16 BOM
/// consumed, invalid sequences replaced rather than refused, and CRLF (or a
/// lone CR, which classic Mac tools still write) folded to LF so every parser
/// after this sees one line ending.
pub fn decode(bytes: &[u8]) -> String {
    let text = if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        utf16(rest, u16::from_le_bytes)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        utf16(rest, u16::from_be_bytes)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text
    }
}

fn utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| unit([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units)
}

/// `hh:mm:ss,mmm` in microseconds — the timing both text formats state, with
/// every part they are written with in practice: comma or dot before the
/// fraction, hours optional, a fraction of one to six digits, a leading minus
/// on a file whose author shifted it too far.
pub fn parse_clock(field: &str) -> Option<i64> {
    let field = field.trim();
    let (sign, field) = match field.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, field),
    };
    let (clock, fraction) = match field.split_once([',', '.']) {
        Some((clock, fraction)) => (clock, fraction),
        None => (field, ""),
    };
    let mut parts = clock.split(':').rev();
    let seconds: i64 = parts.next()?.trim().parse().ok()?;
    let minutes: i64 = parts.next().map_or(Ok(0), |m| m.trim().parse()).ok()?;
    let hours: i64 = parts.next().map_or(Ok(0), |h| h.trim().parse()).ok()?;
    if parts.next().is_some() {
        return None;
    }
    // A fraction is a fraction whatever its length: `.5` is half a second and
    // `.500000` is the same instant.
    let digits: String = fraction
        .chars()
        .take_while(char::is_ascii_digit)
        .take(6)
        .collect();
    let micros = if digits.is_empty() {
        0
    } else {
        let value: i64 = digits.parse().ok()?;
        value * 10i64.pow(6 - digits.len() as u32)
    };
    Some(sign * ((hours * 3600 + minutes * 60 + seconds) * 1_000_000 + micros))
}

/// One cue body's markup as runs. `webvtt` turns on the two tags only WebVTT
/// has — `<v>` and `<c>` — and the inline timestamps that go with them.
pub(crate) fn parse_inline(body: &str, webvtt: bool) -> Vec<Segment> {
    // A stack of open tags, each with the children collected inside it so far;
    // the bottom frame is the cue itself and can never be popped.
    let mut stack: Vec<(Option<Open>, Vec<Segment>)> = vec![(None, Vec::new())];
    let mut text = String::new();
    let mut rest = body;
    while let Some(at) = rest.find('<') {
        text.push_str(&unescape(&rest[..at]));
        let after = &rest[at + 1..];
        let Some(end) = after.find('>') else {
            // No closing bracket anywhere: the rest of the cue is text.
            text.push_str(&unescape(&rest[at..]));
            rest = "";
            break;
        };
        let tag = &after[..end];
        rest = &after[end + 1..];
        match classify(tag, webvtt) {
            Tag::Text => {
                text.push('<');
                text.push_str(&unescape(tag));
                text.push('>');
            }
            Tag::Timestamp(offset_us) => {
                flush(&mut text, &mut stack);
                push(&mut stack, Segment::Timestamp { offset_us });
            }
            Tag::Raw => {
                flush(&mut text, &mut stack);
                push(&mut stack, Segment::Raw(format!("<{tag}>")));
            }
            Tag::Open(open) => {
                flush(&mut text, &mut stack);
                stack.push((Some(open), Vec::new()));
            }
            Tag::Close(name) => {
                flush(&mut text, &mut stack);
                // Close the innermost frame with that name, and everything
                // inside it: `<b><i></b>` closes both, which is what a browser
                // does with the same bytes.
                let found = stack
                    .iter()
                    .rposition(|(open, _)| open.as_ref().is_some_and(|o| o.name == name));
                if let Some(depth) = found {
                    while stack.len() > depth {
                        close_top(&mut stack);
                    }
                }
            }
        }
    }
    text.push_str(&unescape(rest));
    flush(&mut text, &mut stack);
    while stack.len() > 1 {
        close_top(&mut stack);
    }
    stack
        .pop()
        .map(|(_, children)| children)
        .unwrap_or_default()
}

/// An open tag: its name and the arguments the closing wrapper will need.
struct Open {
    name: String,
    color: Option<(u8, u8, u8)>,
    family: Option<String>,
    size: Option<f32>,
    annotation: String,
}

enum Tag {
    Open(Open),
    Close(String),
    Timestamp(i64),
    Raw,
    /// Not a tag at all — a `<` that means `<`.
    Text,
}

fn classify(tag: &str, webvtt: bool) -> Tag {
    let trimmed = tag.trim();
    if trimmed.is_empty() {
        return Tag::Text;
    }
    if let Some(name) = trimmed.strip_prefix('/') {
        let name = name.trim().to_ascii_lowercase();
        let name = name.split(['.', ' ']).next().unwrap_or("").to_owned();
        return Tag::Close(name);
    }
    // `<00:00:01.500>` is a cue timestamp, not an element.
    if webvtt && trimmed.starts_with(|c: char| c.is_ascii_digit()) {
        return match parse_clock(trimmed) {
            Some(offset_us) => Tag::Timestamp(offset_us),
            None => Tag::Text,
        };
    }
    let (head, attrs) = match trimmed.split_once(char::is_whitespace) {
        Some((head, attrs)) => (head, attrs),
        None => (trimmed, ""),
    };
    let (head, classes) = match head.split_once('.') {
        Some((head, classes)) => (head, classes),
        None => (head, ""),
    };
    let name = head.trim_end_matches('/').to_ascii_lowercase();
    match name.as_str() {
        "b" | "i" | "u" | "s" => Tag::Open(Open {
            name,
            color: None,
            family: None,
            size: None,
            annotation: String::new(),
        }),
        "font" => Tag::Open(Open {
            name,
            color: attribute(attrs, "color").and_then(|v| parse_color(&v)),
            family: attribute(attrs, "face"),
            size: attribute(attrs, "size").and_then(|v| v.trim().parse().ok()),
            annotation: String::new(),
        }),
        "v" | "c" if webvtt => Tag::Open(Open {
            annotation: match name.as_str() {
                "v" => attrs.trim().to_owned(),
                _ => classes.replace('.', " "),
            },
            name,
            color: None,
            family: None,
            size: None,
        }),
        _ => Tag::Raw,
    }
}

/// `name="value"` / `name=value` out of a tag's attribute text.
fn attribute(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    loop {
        let at = lower[from..].find(name)? + from;
        let after = attrs[at + name.len()..].trim_start();
        match after.strip_prefix('=') {
            Some(value) => {
                let value = value.trim_start();
                let quoted = value.strip_prefix(['"', '\'']);
                return Some(match quoted {
                    Some(v) => v.split(['"', '\'']).next().unwrap_or(v).to_owned(),
                    None => value
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches('/')
                        .to_owned(),
                });
            }
            None => from = at + name.len(),
        }
    }
}

/// `#rrggbb`, `#rgb`, or one of the sixteen names HTML has always had.
fn parse_color(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        let digits: String = hex.chars().take_while(char::is_ascii_hexdigit).collect();
        let byte = |s: &str| u8::from_str_radix(s, 16).ok();
        return match digits.len() {
            3 => {
                let mut c = digits.chars().map(|c| byte(&format!("{c}{c}")));
                Some((c.next()??, c.next()??, c.next()??))
            }
            6 | 8 => Some((
                byte(&digits[0..2])?,
                byte(&digits[2..4])?,
                byte(&digits[4..6])?,
            )),
            _ => None,
        };
    }
    let named = [
        ("black", (0, 0, 0)),
        ("silver", (192, 192, 192)),
        ("gray", (128, 128, 128)),
        ("grey", (128, 128, 128)),
        ("white", (255, 255, 255)),
        ("maroon", (128, 0, 0)),
        ("red", (255, 0, 0)),
        ("purple", (128, 0, 128)),
        ("fuchsia", (255, 0, 255)),
        ("green", (0, 128, 0)),
        ("lime", (0, 255, 0)),
        ("olive", (128, 128, 0)),
        ("yellow", (255, 255, 0)),
        ("navy", (0, 0, 128)),
        ("blue", (0, 0, 255)),
        ("teal", (0, 128, 128)),
        ("aqua", (0, 255, 255)),
        ("cyan", (0, 255, 255)),
        ("magenta", (255, 0, 255)),
    ];
    let lower = value.to_ascii_lowercase();
    named
        .iter()
        .find(|(name, _)| *name == lower)
        .map(|(_, rgb)| *rgb)
}

/// The handful of entities subtitle text is written with, plus numeric ones.
fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after.find(';').filter(|&e| e <= 10);
        let Some(end) = end else {
            out.push('&');
            rest = after;
            continue;
        };
        let name = &after[..end];
        let replacement = match name.to_ascii_lowercase().as_str() {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "lrm" => Some('\u{200e}'),
            "rlm" => Some('\u{200f}'),
            other => other
                .strip_prefix('#')
                .and_then(|n| match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok(),
                    None => n.parse().ok(),
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(c) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn flush(text: &mut String, stack: &mut [(Option<Open>, Vec<Segment>)]) {
    if !text.is_empty() {
        let taken = std::mem::take(text);
        // A newline inside a cue body is a line break, not whitespace.
        let mut lines = taken.split('\n');
        if let Some(first) = lines.next()
            && !first.is_empty()
        {
            stack
                .last_mut()
                .unwrap()
                .1
                .push(Segment::Text(first.into()));
        }
        for line in lines {
            let children = &mut stack.last_mut().unwrap().1;
            children.push(Segment::LineBreak);
            if !line.is_empty() {
                children.push(Segment::Text(line.into()));
            }
        }
    }
}

fn push(stack: &mut [(Option<Open>, Vec<Segment>)], segment: Segment) {
    stack.last_mut().unwrap().1.push(segment);
}

fn close_top(stack: &mut Vec<(Option<Open>, Vec<Segment>)>) {
    let Some((open, children)) = stack.pop() else {
        return;
    };
    let Some(open) = open else { return };
    let parent = &mut stack.last_mut().unwrap().1;
    match open.name.as_str() {
        "b" => parent.push(Segment::Bold(children)),
        "i" => parent.push(Segment::Italic(children)),
        "u" => parent.push(Segment::Underline(children)),
        "s" => parent.push(Segment::Strike(children)),
        "v" => parent.push(Segment::Voice {
            name: open.annotation,
            children,
        }),
        "c" => parent.push(Segment::Class {
            name: open.annotation,
            children,
        }),
        // `<font>`: whichever of colour and face/size it stated. One that
        // stated neither is a frame with nothing to say, so it is unwrapped
        // rather than kept as an empty node.
        _ => {
            if let Some(rgb) = open.color {
                parent.push(Segment::Color { rgb, children });
            } else if open.family.is_some() || open.size.is_some() {
                parent.push(Segment::Font {
                    family: open.family,
                    size: open.size,
                    children,
                });
            } else {
                parent.extend(children);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::plain_text;

    #[test]
    fn clocks_are_read_in_every_shape_a_file_writes_them() {
        assert_eq!(parse_clock("00:00:01,500"), Some(1_500_000));
        assert_eq!(parse_clock("00:00:01.5"), Some(1_500_000));
        assert_eq!(parse_clock("1:02:03.250"), Some(3_723_250_000));
        // Hours omitted, as WebVTT allows.
        assert_eq!(parse_clock("02:03.000"), Some(123_000_000));
        // Six digits and beyond.
        assert_eq!(parse_clock("0:00:00.123456"), Some(123_456));
        assert_eq!(parse_clock("0:00:00.1234569"), Some(123_456));
        assert_eq!(parse_clock("-0:00:01.000"), Some(-1_000_000));
        assert_eq!(parse_clock("not a time"), None);
        assert_eq!(parse_clock("1:2:3:4.0"), None);
    }

    #[test]
    fn decode_takes_the_bom_and_folds_the_line_endings() {
        assert_eq!(decode(b"\xef\xbb\xbfa\r\nb\rc"), "a\nb\nc");
        assert_eq!(decode(&[0xFF, 0xFE, b'h', 0, b'i', 0]), "hi");
        // Invalid UTF-8 is replaced, never refused.
        assert_eq!(decode(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn markup_nests_and_forgives() {
        let segments = parse_inline("<i>a</i> <b>b", false);
        assert_eq!(plain_text(&segments), "a b");
        assert!(matches!(segments[0], Segment::Italic(_)));
        assert!(matches!(segments[2], Segment::Bold(_)));
        // Mismatched close, stray close, and a `<` that is just a `<`.
        assert_eq!(plain_text(&parse_inline("<b><i>x</b>y", false)), "xy");
        assert_eq!(plain_text(&parse_inline("</i>plain", false)), "plain");
        assert_eq!(plain_text(&parse_inline("a < b", false)), "a < b");
        assert_eq!(
            plain_text(&parse_inline("2 &lt; 3 &amp; 4", false)),
            "2 < 3 & 4"
        );
    }

    #[test]
    fn webvtt_only_tags_are_off_for_srt() {
        let vtt = parse_inline("<v Ann>hi</v>", true);
        assert!(matches!(vtt[0], Segment::Voice { .. }));
        assert_eq!(plain_text(&vtt), "hi");
        let srt = parse_inline("<v Ann>hi", false);
        assert_eq!(plain_text(&srt), "hi");
        assert!(matches!(srt[0], Segment::Raw(_)));
    }

    #[test]
    fn font_colour_is_read_by_hex_and_by_name() {
        let segments = parse_inline("<font color=\"#ff0000\">red</font>", false);
        assert!(matches!(
            segments[0],
            Segment::Color {
                rgb: (255, 0, 0),
                ..
            }
        ));
        let named = parse_inline("<font color=lime>green</font>", false);
        assert!(matches!(
            named[0],
            Segment::Color {
                rgb: (0, 255, 0),
                ..
            }
        ));
    }
}

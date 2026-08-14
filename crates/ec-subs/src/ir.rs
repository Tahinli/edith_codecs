//! The one cue model every subtitle parser in the family answers with.
//!
//! A cue is a time range and a *tree* of inline runs rather than a string:
//! italics, colour, a karaoke beat and a line break are all things a renderer
//! will want and a plain-text caller can drop in one call
//! ([`plain_text`]). Positioning that the format states — WebVTT cue settings,
//! ASS `\pos` and `\an` — rides beside the runs instead of inside them, because
//! it applies to the cue as a whole.

/// One inline run of a cue body.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    /// Literal text.
    Text(String),
    /// Hard line break (SRT/WebVTT newline, ASS `\N`).
    LineBreak,
    /// Bold children (`<b>`, ASS `\b1`).
    Bold(Vec<Segment>),
    /// Italic children (`<i>`, ASS `\i1`).
    Italic(Vec<Segment>),
    /// Underlined children (`<u>`, ASS `\u1`).
    Underline(Vec<Segment>),
    /// Struck-through children (`<s>`, ASS `\s1`).
    Strike(Vec<Segment>),
    /// Children in an explicit colour (`<font color>`, ASS `\c` / `\1c`).
    Color {
        /// Text colour, one byte per channel.
        rgb: (u8, u8, u8),
        /// What the colour applies to.
        children: Vec<Segment>,
    },
    /// Children under a font override (`<font face size>`, ASS `\fn` / `\fs`).
    Font {
        /// Family name; `None` inherits.
        family: Option<String>,
        /// Size in the source format's own units; `None` inherits.
        size: Option<f32>,
        /// What the override applies to.
        children: Vec<Segment>,
    },
    /// WebVTT `<v Speaker>`.
    Voice {
        /// The speaker the annotation names.
        name: String,
        /// What that voice says.
        children: Vec<Segment>,
    },
    /// WebVTT `<c.class>`.
    Class {
        /// Class name without the leading dot.
        name: String,
        /// What the class applies to.
        children: Vec<Segment>,
    },
    /// ASS `{\k<cs>}`: the run is highlighted for `cs` centiseconds.
    Karaoke {
        /// Beat length in centiseconds.
        cs: u32,
        /// The syllable under the beat.
        children: Vec<Segment>,
    },
    /// WebVTT inline timestamp `<00:00:01.500>`, microseconds from stream start.
    Timestamp {
        /// The instant the tag names.
        offset_us: i64,
    },
    /// Markup this parser does not model, kept verbatim so a re-emit stays
    /// faithful and [`plain_text`] can drop it whole. ASS drawing commands
    /// (`{\p1}` … `{\p0}`) land here too: they are shapes, not words.
    Raw(String),
}

/// Horizontal alignment of a cue or a style row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// Start edge of the text direction — left in a left-to-right script.
    #[default]
    Start,
    /// Centred.
    Center,
    /// End edge of the text direction.
    End,
    /// Left edge whatever the text direction.
    Left,
    /// Right edge whatever the text direction.
    Right,
}

/// Where the format asked for the cue to be drawn.
///
/// The units are the source's own: WebVTT `position`/`line`/`size` are
/// percentages of the viewport, ASS `\pos` is pixels in the `PlayResX` ×
/// `PlayResY` canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CuePosition {
    /// Horizontal position, or `None` for the format default.
    pub x: Option<f32>,
    /// Vertical position, or `None` for the format default.
    pub y: Option<f32>,
    /// Horizontal alignment of the cue's lines.
    pub align: TextAlign,
    /// WebVTT `size:N%`; meaningless for ASS.
    pub size: Option<f32>,
}

/// A named style a cue can reference — an ASS `Style:` row, essentially.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubtitleStyle {
    /// Style name, which is what [`SubtitleCue::style_ref`] holds.
    pub name: String,
    /// Font family.
    pub font_family: Option<String>,
    /// Font size in the source's units.
    pub font_size: Option<f32>,
    /// Fill colour, RGBA.
    pub primary_color: Option<(u8, u8, u8, u8)>,
    /// Outline colour, RGBA.
    pub outline_color: Option<(u8, u8, u8, u8)>,
    /// Box/shadow colour, RGBA.
    pub back_color: Option<(u8, u8, u8, u8)>,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Strikeout.
    pub strike: bool,
    /// Alignment of the style's lines.
    pub align: TextAlign,
    /// Left margin.
    pub margin_l: Option<i32>,
    /// Right margin.
    pub margin_r: Option<i32>,
    /// Vertical margin.
    pub margin_v: Option<i32>,
    /// Outline thickness.
    pub outline: Option<f32>,
    /// Shadow depth.
    pub shadow: Option<f32>,
}

impl SubtitleStyle {
    /// An otherwise defaulted style called `name`.
    pub fn new(name: impl Into<String>) -> SubtitleStyle {
        SubtitleStyle {
            name: name.into(),
            ..SubtitleStyle::default()
        }
    }
}

/// One cue: when it is up, what it says, and how it was asked to be drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubtitleCue {
    /// Start, microseconds from the start of the media.
    pub start_us: i64,
    /// End, microseconds from the start of the media. Never before
    /// [`start_us`](Self::start_us): a backwards pair is clamped at parse.
    pub end_us: i64,
    /// The style this cue names, when the format has named styles.
    pub style_ref: Option<String>,
    /// Positioning the cue itself states.
    pub positioning: Option<CuePosition>,
    /// The body, as runs.
    pub segments: Vec<Segment>,
}

/// Which on-disk flavour a track was parsed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    /// SubRip (`.srt`).
    Srt,
    /// WebVTT (`.vtt`).
    WebVtt,
    /// Advanced SubStation Alpha or its SSA predecessor.
    AssOrSsa,
}

/// A parsed subtitle file: where it came from, its styles, its cues.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubtitleTrack {
    /// The flavour, when the parser knows it.
    pub source: Option<SourceFormat>,
    /// Named styles, in file order.
    pub styles: Vec<SubtitleStyle>,
    /// Cues, in file order — *not* sorted by time, because a file's own order
    /// is information (an overlapping pair is a pair, not an error).
    pub cues: Vec<SubtitleCue>,
    /// Track-level keys: `title`, `play_res_x`, `wrap_style`, `header`, …
    pub metadata: Vec<(String, String)>,
    /// Header bytes worth replaying: the ASS script header, the WebVTT
    /// preamble. What a Matroska `CodecPrivate` holds for this track.
    pub extradata: Vec<u8>,
    /// Blocks the parser could not read and skipped. Never an error on its own
    /// — a file with one torn cue is still a file of cues — but a caller that
    /// wants to say so has the count.
    pub skipped: usize,
}

impl SubtitleTrack {
    /// An empty track.
    pub fn new() -> SubtitleTrack {
        SubtitleTrack::default()
    }

    /// The same track, tagged with the format it was read from.
    pub fn with_source(mut self, source: SourceFormat) -> SubtitleTrack {
        self.source = Some(source);
        self
    }

    /// A style by name.
    pub fn style(&self, name: &str) -> Option<&SubtitleStyle> {
        self.styles.iter().find(|s| s.name == name)
    }
}

/// The words of a cue body, markup resolved and dropped: `\n` between lines,
/// nothing at all for a run that is a drawing or an override this does not
/// model.
pub fn plain_text(segments: &[Segment]) -> String {
    let mut out = String::new();
    append_plain(segments, &mut out);
    out
}

fn append_plain(segments: &[Segment], out: &mut String) {
    for segment in segments {
        match segment {
            Segment::Text(s) => out.push_str(s),
            Segment::LineBreak => out.push('\n'),
            Segment::Bold(children)
            | Segment::Italic(children)
            | Segment::Underline(children)
            | Segment::Strike(children) => append_plain(children, out),
            Segment::Color { children, .. }
            | Segment::Font { children, .. }
            | Segment::Voice { children, .. }
            | Segment::Class { children, .. }
            | Segment::Karaoke { children, .. } => append_plain(children, out),
            Segment::Timestamp { .. } | Segment::Raw(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_walks_the_tree_and_drops_what_cannot_be_said() {
        let segments = vec![
            Segment::Italic(vec![Segment::Text("tilted".into())]),
            Segment::Text(" and ".into()),
            Segment::Color {
                rgb: (255, 0, 0),
                children: vec![Segment::Text("red".into())],
            },
            Segment::LineBreak,
            Segment::Raw("{\\p1}m 0 0 l 1 1".into()),
            Segment::Timestamp { offset_us: 5 },
            Segment::Karaoke {
                cs: 42,
                children: vec![Segment::Text("ka".into())],
            },
        ];
        assert_eq!(plain_text(&segments), "tilted and red\nka");
    }
}

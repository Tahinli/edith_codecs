//! ASS parity with ffmpeg, and with the Matroska rebuild edith runs, on the synthetic scripts in
//! `tests/data/`.
//!
//! ffmpeg is the reference. `ffprobe -show_packets` gives every event's start and duration, and
//! `-c:s text -f srt` gives its words with every override tag stripped. ffmpeg emits events in
//! start-time order, ties kept in file order, so ec-ass's cues are sorted the same way (stably)
//! before comparing. Each cue must match on start and end in milliseconds, on order, and on text.
//!
//! `reordered_format.ass` is not compared with ffmpeg. Its demuxer produces no packets at all for a
//! script whose Events `Format:` line puts Start before Layer, so there is nothing to compare. Its
//! expectations are written out by hand instead.
//!
//! The ffmpeg tests skip loudly when `ffmpeg` or `ffprobe` is not on PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use ec_ass::parse;
use ec_subs::{Segment, SubtitleCue, TextAlign, plain_text};

/// Fixtures ffmpeg can demux, all compared end to end.
const FFMPEG_FIXTURES: &[&str] = &[
    "styles_tags.ass",
    "layers_overlap.ass",
    "multiline_wrap0.ass",
    "multiline_wrap2.ass",
    "timing_edges.ass",
    "unicode_sections.ass",
    "karaoke_colours.ass",
];

fn data_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn fixture(name: &str) -> Vec<u8> {
    let path = data_path(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn tool_present(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Both reference tools, or `None` (with a loud note) when either is missing.
fn ffmpeg_available() -> bool {
    if tool_present("ffmpeg") && tool_present("ffprobe") {
        return true;
    }
    eprintln!("SKIP: ffmpeg/ffprobe not on PATH; ASS parity with ffmpeg not checked");
    false
}

/// Seconds in ffprobe's decimal form to whole milliseconds.
fn ms(seconds: &str) -> i64 {
    (seconds.trim().parse::<f64>().expect("seconds") * 1000.0).round() as i64
}

/// `(start_ms, end_ms)` per packet, in the order ffprobe lists them.
fn ffprobe_spans(path: &Path) -> Vec<(i64, i64)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "s:0",
            "-show_entries",
            "packet=pts_time,duration_time",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(
        out.status.success(),
        "ffprobe failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| {
            let mut fields = line.split(',');
            let start = ms(fields.next().expect("pts_time"));
            // A zero-length event reports no duration (`N/A`).
            let duration = fields
                .next()
                .and_then(|d| d.trim().parse::<f64>().ok())
                .map_or(0, |d| (d * 1000.0).round() as i64);
            (start, start + duration)
        })
        .collect()
}

/// ffmpeg's SRT rendering of the script's cues, words only.
fn ffmpeg_srt(path: &Path) -> String {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:s:0", "-c:s", "text", "-f", "srt", "-"])
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("srt is utf-8")
}

/// `HH:MM:SS,mmm` to milliseconds.
fn srt_ms(field: &str) -> i64 {
    let (clock, millis) = field.trim().split_once(',').expect("srt comma");
    let mut parts = clock
        .split(':')
        .map(|p| p.parse::<i64>().expect("srt digits"));
    let (h, m, s) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    ((h * 60 + m) * 60 + s) * 1000 + millis.parse::<i64>().expect("srt millis")
}

/// `(start_ms, end_ms, text)` per cue of an SRT document. Each block is an index line, a timing
/// line, its text lines, then one blank separator before the next index line. A text that ends
/// in a line break keeps that blank line, so the separator is cut by position, not by content.
fn srt_cues(srt: &str) -> Vec<(i64, i64, String)> {
    let lines: Vec<&str> = srt.lines().collect();
    let timings: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(" --> "))
        .map(|(i, _)| i)
        .collect();
    timings
        .iter()
        .enumerate()
        .map(|(k, &t)| {
            let (start, end) = lines[t].split_once(" --> ").expect("timing line");
            let text_end = match timings.get(k + 1) {
                // Next timing is at `next`: `next - 1` is its index line, `next - 2` the separator.
                Some(&next) => next - 2,
                None => lines.len() - 1,
            };
            let text = lines[t + 1..text_end.max(t + 1)].join("\n");
            (srt_ms(start), srt_ms(end), text)
        })
        .collect()
}

/// ec-ass's cues as `(start_ms, end_ms, text)`, in ffmpeg's order.
fn ours_in_ffmpeg_order(cues: &[SubtitleCue]) -> Vec<(i64, i64, String)> {
    let mut sorted: Vec<&SubtitleCue> = cues.iter().collect();
    sorted.sort_by_key(|c| c.start_us);
    sorted
        .iter()
        .map(|c| (c.start_us / 1000, c.end_us / 1000, plain_text(&c.segments)))
        .collect()
}

#[test]
fn cues_match_ffmpeg_timing_order_and_text() {
    if !ffmpeg_available() {
        return;
    }
    for name in FFMPEG_FIXTURES {
        let path = data_path(name);
        let track = parse(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ours = ours_in_ffmpeg_order(&track.cues);

        let packets = ffprobe_spans(&path);
        assert_eq!(packets.len(), ours.len(), "{name}: event count");
        for (i, (packet, ours)) in packets.iter().zip(&ours).enumerate() {
            assert_eq!(
                *packet,
                (ours.0, ours.1),
                "{name}: event {i} start/end (ms)"
            );
        }

        let reference = srt_cues(&ffmpeg_srt(&path));
        assert_eq!(reference.len(), ours.len(), "{name}: srt cue count");
        for (i, (theirs, mine)) in reference.iter().zip(&ours).enumerate() {
            assert_eq!(theirs, mine, "{name}: event {i} (start, end, text)");
        }
    }
}

/// Edith rebuilds a Matroska ASS track as a plain script: the `CodecPrivate` header, then one
/// `Dialogue:` row per block, with the block's timing and its `ReadOrder` dropped. The same
/// rebuild must parse to the same cues as the script itself.
#[test]
fn matroska_style_blocks_rebuild_to_the_same_cues() {
    for name in FFMPEG_FIXTURES {
        let bytes = fixture(name);
        let direct = parse(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));

        // Matroska form: header as CodecPrivate, then `ReadOrder,Layer,...` blocks.
        let text = String::from_utf8_lossy(&bytes);
        let mut rebuilt = direct.extradata.clone();
        let mut read_order = 0;
        for line in text.lines() {
            let Some(body) = line.trim().strip_prefix("Dialogue:") else {
                continue;
            };
            let mut fields = body.trim_start().splitn(4, ',');
            let (layer, start, end, rest) = (
                fields.next().unwrap(),
                fields.next().unwrap(),
                fields.next().unwrap(),
                fields.next().unwrap(),
            );
            let block = format!("{read_order},{layer},{rest}");
            read_order += 1;

            // Edith's side: the block is split back into timing and the rest of the row.
            let mut block_fields = block.splitn(3, ',');
            let _read_order = block_fields.next().unwrap();
            let layer = block_fields.next().unwrap();
            let rest = block_fields.next().unwrap();
            rebuilt
                .extend_from_slice(format!("Dialogue: {layer},{start},{end},{rest}\n").as_bytes());
        }

        let again = parse(&rebuilt).unwrap_or_else(|e| panic!("{name}: rebuilt: {e}"));
        assert_eq!(again.cues.len(), direct.cues.len(), "{name}: cue count");
        for (i, (a, b)) in direct.cues.iter().zip(&again.cues).enumerate() {
            assert_eq!(
                (a.start_us, a.end_us, &a.segments, a.positioning),
                (b.start_us, b.end_us, &b.segments, b.positioning),
                "{name}: cue {i}"
            );
        }
    }
}

/// A Format line that reorders the columns is read by name, not by position.
#[test]
fn reordered_format_is_read_by_column_name() {
    let track = parse(&fixture("reordered_format.ass")).unwrap();

    let top = track.style("Reordered Top").expect("style by name");
    assert_eq!(top.font_size, Some(44.0));
    assert_eq!(top.font_family.as_deref(), Some("Verdana"));
    assert!(top.bold && !top.italic);
    assert_eq!(top.align, TextAlign::Center);
    let bottom = track.style("Reordered Bottom").expect("style by name");
    assert!(bottom.italic && !bottom.bold);
    assert_eq!(bottom.primary_color, Some((255, 255, 0, 255)));

    let texts: Vec<String> = track.cues.iter().map(|c| plain_text(&c.segments)).collect();
    assert_eq!(
        texts,
        [
            "first row in reordered columns",
            "second row, comma kept in text",
            "overlapping third row",
            "bold last",
        ]
    );
    let starts: Vec<(i64, i64)> = track.cues.iter().map(|c| (c.start_us, c.end_us)).collect();
    assert_eq!(
        starts,
        [
            (1_000_000, 2_500_000),
            (2_500_000, 4_000_000),
            (3_000_000, 5_000_000),
            (5_000_000, 6_000_000),
        ]
    );
    assert_eq!(track.cues[0].style_ref.as_deref(), Some("Reordered Top"));
    assert_eq!(track.cues[1].style_ref.as_deref(), Some("Reordered Bottom"));
    assert_eq!(track.cues[2].positioning.unwrap().align, TextAlign::Center);
    let last = track.cues[3].positioning.unwrap();
    assert_eq!((last.x, last.y), (Some(10.0), Some(20.0)));
    assert_eq!(
        track.cues[3].segments,
        vec![Segment::Bold(vec![Segment::Text("bold last".into())])]
    );
}

/// Override tags this parser does not model reach the caller verbatim as `Raw`, with each
/// unmodelled tag's own text, including tags nested in `\t(...)`. A `\fs` inside `\t(...)` must
/// not be read as a font size.
#[test]
fn unmodelled_override_tags_survive_as_verbatim_raw() {
    let doc = "[Events]\n\
        Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
        Dialogue: 0,0:00:00.00,0:00:01.00,,,0,0,0,,{\\t(0,500,\\fs20)}grow\n\
        Dialogue: 0,0:00:01.00,0:00:02.00,,,0,0,0,,{\\blur3\\bord2\\frz30\\fscx120}wide\n\
        Dialogue: 0,0:00:02.00,0:00:03.00,,,0,0,0,,{\\move(10,10,200,200,0,500)}moving\n\
        Dialogue: 0,0:00:03.00,0:00:04.00,,,0,0,0,,{\\clip(0,0,100,100)\\org(5,5)}clipped\n\
        Dialogue: 0,0:00:04.00,0:00:05.00,,,0,0,0,,{\\p1}m 0 0 l 10 10{\\p0}words\n";
    let track = parse(doc.as_bytes()).unwrap();
    let segments: Vec<&Vec<Segment>> = track.cues.iter().map(|c| &c.segments).collect();

    assert_eq!(
        *segments[0],
        vec![
            Segment::Raw("{\\t(0,500,\\fs20)}".into()),
            Segment::Text("grow".into()),
        ]
    );
    assert_eq!(
        *segments[1],
        vec![
            Segment::Raw("{\\blur3\\bord2\\frz30\\fscx120}".into()),
            Segment::Text("wide".into()),
        ]
    );
    assert_eq!(
        *segments[2],
        vec![
            Segment::Raw("{\\move(10,10,200,200,0,500)}".into()),
            Segment::Text("moving".into()),
        ]
    );
    assert_eq!(
        *segments[3],
        vec![
            Segment::Raw("{\\clip(0,0,100,100)\\org(5,5)}".into()),
            Segment::Text("clipped".into()),
        ]
    );
    // Drawing commands are kept whole as Raw, and are not words.
    assert_eq!(
        *segments[4],
        vec![
            Segment::Raw("m 0 0 l 10 10".into()),
            Segment::Text("words".into()),
        ]
    );
    assert_eq!(
        track.cues[2].positioning.unwrap().x,
        Some(10.0),
        "\\move starts where \\pos would put it"
    );
}

//! Inputs the fuzz review and the read-through found to panic before the fixes in `lib.rs`.
//! Each input is inlined; none needs a fixture.

use ec_subs::{SourceFormat, plain_text};

const EVENTS_HEADER: &str = "[Events]\n\
    Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

/// `(code - 1)` on an i32 that is `i32::MIN` panicked in debug. A style row and a `\an` override
/// both state a number a file can write.
#[test]
fn an_alignment_code_at_i32_min_does_not_overflow() {
    let doc = format!(
        "[V4+ Styles]\nFormat: Name, Alignment\nStyle: Default,-2147483648\n\n{EVENTS_HEADER}"
    );
    let track = ec_ass::parse(doc.as_bytes()).unwrap();
    assert_eq!(track.styles.len(), 1);

    let doc = format!(
        "{EVENTS_HEADER}Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,{{\\an-2147483648}}hi\n"
    );
    let track = ec_ass::parse(doc.as_bytes()).unwrap();
    assert_eq!(plain_text(&track.cues[0].segments), "hi");
}

/// A Start clock with an hours field that overflows microseconds: the Dialogue row is skipped and
/// counted, not a panic.
#[test]
fn a_dialogue_clock_too_large_is_skipped() {
    let doc = format!(
        "{EVENTS_HEADER}Dialogue: 0,99999999999999:00:00.00,0:00:01.00,Default,,0,0,0,,lost\n\
         Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,kept\n"
    );
    let track = ec_ass::parse(doc.as_bytes()).unwrap();
    assert_eq!(track.cues.len(), 1);
    assert_eq!(track.skipped, 1);
}

/// The WrapStyle is read from the header once. The first `WrapStyle:` line still governs `\n`,
/// as it did when the metadata was searched for every Dialogue row.
#[test]
fn the_first_wrap_style_line_governs_soft_breaks() {
    let doc = format!(
        "[Script Info]\nWrapStyle: 2\nWrapStyle: 0\n{EVENTS_HEADER}\
         Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,a\\nb\n"
    );
    let track = ec_ass::parse(doc.as_bytes()).unwrap();
    assert!(
        track.cues[0]
            .segments
            .iter()
            .any(|s| matches!(s, ec_subs::Segment::LineBreak))
    );
    assert_eq!(track.source, Some(SourceFormat::AssOrSsa));
}

/// A long header and many Dialogue rows parse whole, every row accounted for. The WrapStyle lookup
/// used to rescan the whole header for every row.
#[test]
fn a_long_header_and_many_rows_parse_whole() {
    let mut doc = String::from("[Script Info]\nWrapStyle: 0\n");
    for i in 0..20_000 {
        doc.push_str(&format!("Key{i}: value\n"));
    }
    doc.push_str(EVENTS_HEADER);
    for i in 0..5_000 {
        doc.push_str(&format!(
            "Dialogue: 0,0:00:{:02}.00,0:00:{:02}.50,Default,,0,0,0,,row {i}\n",
            i % 60,
            i % 60
        ));
    }
    let track = ec_ass::parse(doc.as_bytes()).unwrap();
    assert_eq!(track.cues.len(), 5_000);
    assert_eq!(track.skipped, 0);
}

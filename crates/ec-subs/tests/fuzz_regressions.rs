//! Inputs the fuzz review and the read-through found to panic, or to cost quadratic time or
//! unbounded stack, before the fixes in `markup.rs`. Each input is inlined; none needs a fixture.

use ec_subs::{plain_text, srt, webvtt};

/// Hours and minutes large enough that `hours * 3600 * 1_000_000` leaves an i64. Panicked in
/// debug (overflow checks) before the sums were checked.
#[test]
fn a_clock_too_large_for_microseconds_is_refused_not_overflowed() {
    assert_eq!(ec_subs::parse_clock("99999999999999:00:00"), None);
    assert_eq!(ec_subs::parse_clock("0:99999999999999:00"), None);
    assert_eq!(ec_subs::parse_clock("-99999999999999:00:00.5"), None);
    // The largest clock that does fit still parses.
    assert!(ec_subs::parse_clock("2562047:00:00").is_some());
}

#[test]
fn a_cue_with_an_overflowing_clock_is_skipped_and_counted() {
    let doc = b"1\n99999999999999:00:00,000 --> 99999999999999:00:01,000\nlost\n\n\
                2\n00:00:01,000 --> 00:00:02,000\nkept\n";
    let track = srt::parse(doc).unwrap();
    assert_eq!(track.cues.len(), 1);
    assert_eq!(track.skipped, 1);
    assert_eq!(plain_text(&track.cues[0].segments), "kept");
}

/// A cue body of a thousand-thousand open tags used to be a tree that deep, so closing and
/// walking it was quadratic and `plain_text` recursed past the stack. Nesting is now capped and the
/// text survives.
#[test]
fn deep_nesting_is_capped_and_the_text_survives() {
    let mut doc = b"1\n00:00:01,000 --> 00:00:02,000\n".to_vec();
    doc.extend(std::iter::repeat_n(b"<i>".as_slice(), 200_000).flatten());
    doc.extend_from_slice(b"deep");
    doc.extend(std::iter::repeat_n(b"</i>".as_slice(), 200_000).flatten());
    let track = srt::parse(&doc).unwrap();
    assert_eq!(plain_text(&track.cues[0].segments), "deep");

    let mut vtt = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n".to_vec();
    vtt.extend(std::iter::repeat_n(b"<b>".as_slice(), 200_000).flatten());
    vtt.extend_from_slice(b"x");
    let track = webvtt::parse(&vtt).unwrap();
    assert_eq!(plain_text(&track.cues[0].segments), "x");
}

/// A bare `&` with no `;` anywhere scanned to the end of the text for each one: quadratic. The
/// entity name is at most ten bytes, so the scan is bounded now and the text comes back unchanged.
#[test]
fn a_run_of_bare_ampersands_is_linear_and_unchanged() {
    let amps = "&".repeat(100_000);
    let doc = format!("1\n00:00:01,000 --> 00:00:02,000\n{amps}\n");
    let track = srt::parse(doc.as_bytes()).unwrap();
    assert_eq!(plain_text(&track.cues[0].segments), amps);
    // A real entity after a long run still decodes.
    let doc = format!("1\n00:00:01,000 --> 00:00:02,000\n{amps}&amp;\n");
    let track = srt::parse(doc.as_bytes()).unwrap();
    assert!(plain_text(&track.cues[0].segments).ends_with("&&"));
}

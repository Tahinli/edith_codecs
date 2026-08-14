//! The three parsers against streams taken out of the user's own library.
//!
//! Hand-written cases live in the crates' unit tests; what cannot be written by
//! hand is a disc's PGS display set or a fansub's thousand-line ASS script.
//! `scripts/gen-subtitle-fixtures.sh` extracts them (fixtures are gitignored,
//! the script is not), and every test here *skips* when its fixture is absent
//! so a fresh clone still runs green.

use std::path::PathBuf;

use ec_subs::{SourceFormat, plain_text};

fn fixture(name: &str) -> Option<Vec<u8>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/subs")
        .join(name);
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(_) => {
            eprintln!("skipping: no {name} — run scripts/gen-subtitle-fixtures.sh");
            None
        }
    }
}

#[test]
fn a_real_subrip_track_parses_whole() {
    let Some(bytes) = fixture("real.srt") else {
        return;
    };
    let track = ec_subs::srt::parse(&bytes).unwrap();
    assert_eq!(track.source, Some(SourceFormat::Srt));
    assert!(track.cues.len() > 100, "{} cues", track.cues.len());
    assert_eq!(track.skipped, 0, "a whole file should have no torn blocks");
    // Every cue is up for a positive time and says something.
    for cue in &track.cues {
        assert!(cue.end_us >= cue.start_us);
        assert!(
            !plain_text(&cue.segments).trim().is_empty(),
            "empty cue at {}us",
            cue.start_us
        );
    }
    // A subtitle track runs forwards.
    assert!(
        track
            .cues
            .windows(2)
            .all(|w| w[1].start_us >= w[0].start_us)
    );
}

#[test]
fn a_real_ass_script_parses_whole() {
    let Some(bytes) = fixture("real.ass") else {
        return;
    };
    let track = ec_ass::parse(&bytes).unwrap();
    assert_eq!(track.source, Some(SourceFormat::AssOrSsa));
    assert!(!track.styles.is_empty(), "a script declares styles");
    assert!(track.cues.len() > 100, "{} cues", track.cues.len());
    assert_eq!(track.skipped, 0, "a whole script should have no torn rows");
    // Every cue names a style the script declared, and most say words: a
    // signs-and-songs script has drawing-only lines, which are not words.
    let with_text = track
        .cues
        .iter()
        .filter(|c| !plain_text(&c.segments).trim().is_empty())
        .count();
    assert!(
        with_text * 10 > track.cues.len() * 9,
        "{with_text} of {}",
        track.cues.len()
    );
    for cue in &track.cues {
        assert!(cue.end_us >= cue.start_us);
        if let Some(style) = &cue.style_ref {
            assert!(
                track.style(style).is_some(),
                "cue names undeclared style {style:?}"
            );
        }
        // Override tags never leak into the words.
        let text = plain_text(&cue.segments);
        assert!(!text.contains("{\\"), "override tag in text: {text:?}");
    }
    // The header is replayable as a Matroska `CodecPrivate`.
    assert!(track.extradata.windows(8).any(|w| w == b"[Events]"));
}

#[test]
fn a_real_pgs_stream_decodes_to_plausible_bitmaps() {
    let Some(bytes) = fixture("pgs-1080p.sup") else {
        return;
    };
    let mut decoder = ec_pgs::PgsDecoder::new();
    decoder.push(&bytes).unwrap();
    let mut frames = 0;
    let mut painted = 0;
    let mut coverage_max = 0.0f64;
    while let Some(frame) = decoder.take_frame() {
        frames += 1;
        // A disc composes against its own video frame, and this one is HD.
        assert_eq!((frame.width, frame.height), (1920, 1080));
        let stride = frame.planes[0].stride;
        assert_eq!(stride, 1920 * 4);
        let opaque = frame.planes[0]
            .data
            .chunks_exact(4)
            .filter(|p| p[3] > 0)
            .count();
        if opaque == 0 {
            // An erase display set: a set that composes nothing at all.
            continue;
        }
        painted += 1;
        let coverage = opaque as f64 / (1920.0 * 1080.0);
        coverage_max = coverage_max.max(coverage);
        // Subtitles are lines of text over a film: ink, not a filled canvas.
        assert!(
            coverage > 0.000_1 && coverage < 0.25,
            "display set {frames} covers {:.3}% of the canvas",
            coverage * 100.0
        );
        // The ink sits in the lower or upper band, never as a single stray row.
        let rows = (0..1080)
            .filter(|y| {
                frame.planes[0].data[y * stride..(y + 1) * stride]
                    .chunks_exact(4)
                    .any(|p| p[3] > 0)
            })
            .count();
        assert!(rows > 4, "display set {frames} paints {rows} rows");
    }
    assert!(frames > 100, "{frames} display sets");
    assert!(
        painted * 3 > frames,
        "{painted} of {frames} display sets painted anything"
    );
    assert!(coverage_max < 0.25, "widest cue covers {coverage_max}");
    eprintln!("pgs: {frames} display sets, {painted} painted, widest {coverage_max:.4}");
}

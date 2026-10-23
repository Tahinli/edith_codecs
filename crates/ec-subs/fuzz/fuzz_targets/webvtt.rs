#![no_main]

use ec_subs::{SubtitleTrack, plain_text, webvtt};
use libfuzzer_sys::fuzz_target;

/// Every cue the parser returns is well-formed: the end is never before the start, and the text
/// flattens without a panic.
fn check(track: &SubtitleTrack) {
    for cue in &track.cues {
        assert!(cue.end_us >= cue.start_us);
        let _ = plain_text(&cue.segments);
        if let Some(name) = &cue.style_ref {
            let _ = track.style(name);
        }
    }
}

// WebVTT, bytes and lossy text. The `WEBVTT` header is optional in the parser, so the input is
// also tried after a header, which reaches the cue and settings code on every input.
fuzz_target!(|data: &[u8]| {
    if let Ok(track) = webvtt::parse(data) {
        check(&track);
    }
    let text = String::from_utf8_lossy(data);
    if let Ok(track) = webvtt::parse(text.as_bytes()) {
        check(&track);
    }
    let mut headed = b"WEBVTT\n\n".to_vec();
    headed.extend_from_slice(data);
    if let Ok(track) = webvtt::parse(&headed) {
        check(&track);
    }
});

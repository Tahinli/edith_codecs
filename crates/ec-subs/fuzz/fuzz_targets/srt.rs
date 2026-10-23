#![no_main]

use ec_subs::{SubtitleTrack, plain_text, srt};
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

// SubRip, once with the bytes as they are (invalid UTF-8 and UTF-16 BOMs included) and once with
// the lossy-UTF-8 text a caller holding a `String` would hand over.
fuzz_target!(|data: &[u8]| {
    if let Ok(track) = srt::parse(data) {
        check(&track);
    }
    let text = String::from_utf8_lossy(data);
    if let Ok(track) = srt::parse(text.as_bytes()) {
        check(&track);
    }
});

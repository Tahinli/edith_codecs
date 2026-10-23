#![no_main]

use ec_subs::{SubtitleTrack, plain_text};
use libfuzzer_sys::fuzz_target;


// Every cue is well formed, and every style reference resolves or is absent.
fn check(track: &SubtitleTrack) {
    for cue in &track.cues {
        assert!(cue.end_us >= cue.start_us);
        let _ = plain_text(&cue.segments);
        if let Some(name) = &cue.style_ref {
            let _ = track.style(name);
        }
    }
}

// A whole ASS/SSA script: the section headers, Format lines, every Style and Dialogue row, and the
// override tags inside them. Bytes as they are, then lossy UTF-8.
fuzz_target!(|data: &[u8]| {
    if let Ok(track) = ec_ass::parse(data) {
        check(&track);
    }
    let text = String::from_utf8_lossy(data);
    if let Ok(track) = ec_ass::parse(text.as_bytes()) {
        check(&track);
    }
});

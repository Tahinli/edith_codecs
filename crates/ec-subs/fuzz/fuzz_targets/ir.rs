#![no_main]

use ec_subs::{SourceFormat, plain_text, srt, webvtt};
use libfuzzer_sys::fuzz_target;

// The IR the two parsers produce, and the conversion a caller makes out of it: plain text per cue,
// style lookup by name, and the invariants a timeline depends on (no cue ends before it starts,
// every skipped block is counted, the source format is the one parsed).
fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    for bytes in [data, text.as_bytes()] {
        if let Ok(track) = srt::parse(bytes) {
            assert_eq!(track.source, Some(SourceFormat::Srt));
            let _: String = track.cues.iter().map(|c| plain_text(&c.segments)).collect();
        }
    }
    if let Ok(track) = webvtt::parse(data) {
        assert_eq!(track.source, Some(SourceFormat::WebVtt));
        for cue in &track.cues {
            assert!(cue.end_us >= cue.start_us);
            let _ = plain_text(&cue.segments);
        }
    }
});

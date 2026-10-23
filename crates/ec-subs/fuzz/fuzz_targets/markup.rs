#![no_main]

use ec_subs::{plain_text, srt, webvtt};
use libfuzzer_sys::fuzz_target;

// The markup both formats share, reached through a one-cue document so the input is always a cue
// body: the tag stack, entities, colour and font attributes, and (WebVTT) voices, classes and
// inline timestamps. Also the byte and clock helpers on the raw input.
fuzz_target!(|data: &[u8]| {
    let text = ec_subs::decode(data);
    for field in text.split_whitespace() {
        let _ = ec_subs::parse_clock(field);
    }
    let _ = ec_subs::parse_clock(&text);

    let mut srt_doc = b"1\n00:00:01,000 --> 00:00:02,000\n".to_vec();
    srt_doc.extend_from_slice(data);
    if let Ok(track) = srt::parse(&srt_doc) {
        for cue in &track.cues {
            let _ = plain_text(&cue.segments);
        }
    }
    let mut vtt_doc = b"WEBVTT\n\n00:00:01.000 --> 00:00:02.000\n".to_vec();
    vtt_doc.extend_from_slice(data);
    if let Ok(track) = webvtt::parse(&vtt_doc) {
        for cue in &track.cues {
            let _ = plain_text(&cue.segments);
        }
    }
});

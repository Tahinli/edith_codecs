#![no_main]

use ec_subs::plain_text;
use libfuzzer_sys::fuzz_target;

// The `[Events]` table with the input as one Dialogue row, so Start/End/Style/Text splitting and
// the clock parse are reached on every input. Whole-document rows without a Format line are
// covered by the script target.
fuzz_target!(|data: &[u8]| {
    let mut doc = b"[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: ".to_vec();
    doc.extend_from_slice(data);
    if let Ok(track) = ec_ass::parse(&doc) {
        for cue in &track.cues {
            assert!(cue.end_us >= cue.start_us);
            let _ = plain_text(&cue.segments);
        }
    }
});

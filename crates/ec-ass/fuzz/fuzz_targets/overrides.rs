#![no_main]

use ec_subs::plain_text;
use libfuzzer_sys::fuzz_target;

// The Text field of a Dialogue row: override blocks, drawings, escapes and karaoke. The first
// byte selects the WrapStyle (0 to 3), which decides what `\n` means, and the rest is the Text
// field, wrapped so it is always the last column.
fuzz_target!(|data: &[u8]| {
    let Some((&wrap, text)) = data.split_first() else {
        return;
    };
    let mut doc = format!(
        "[Script Info]\nWrapStyle: {}\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,",
        wrap % 4
    )
    .into_bytes();
    doc.extend_from_slice(text);
    if let Ok(track) = ec_ass::parse(&doc) {
        for cue in &track.cues {
            let _ = plain_text(&cue.segments);
        }
    }
});

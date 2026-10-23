#![no_main]

use libfuzzer_sys::fuzz_target;

// The `[V4+ Styles]` and `[V4 Styles]` tables with the input as the row set. The first byte picks
// the table flavour, so the SSA alignment codes and the V4+ ones both get the same rows. A
// `Format:` line is omitted on odd selectors to reach the built-in column order too.
fuzz_target!(|data: &[u8]| {
    let Some((&select, rows)) = data.split_first() else {
        return;
    };
    let section = if select & 1 == 0 { "[V4+ Styles]" } else { "[V4 Styles]" };
    let format = if select & 2 == 0 {
        "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n"
    } else {
        ""
    };
    let mut doc = format!("{section}\n{format}").into_bytes();
    doc.extend_from_slice(b"Style: ");
    doc.extend_from_slice(rows);
    if let Ok(track) = ec_ass::parse(&doc) {
        for style in &track.styles {
            let _ = track.style(&style.name);
        }
    }
});

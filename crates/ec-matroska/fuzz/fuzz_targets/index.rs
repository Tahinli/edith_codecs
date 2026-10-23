#![no_main]

use ec_matroska::fuzz_api;
use libfuzzer_sys::fuzz_target;

// The seek index's inputs: `Cues` and `SeekHead` bodies. The first byte picks which
// (0 Cues, 1 SeekHead), the next 8 bytes are the segment's file offset that Cue
// positions are relative to, and the rest is the element body.
fuzz_target!(|data: &[u8]| {
    let Some((&which, rest)) = data.split_first() else {
        return;
    };
    let Some((segment, body)) = rest.split_at_checked(8) else {
        return;
    };
    let segment = u64::from_le_bytes(segment.try_into().unwrap_or([0; 8]));
    match which & 1 {
        0 => {
            let _ = fuzz_api::cues(body, segment);
        }
        _ => {
            let _ = fuzz_api::seek_head(body);
        }
    }
});

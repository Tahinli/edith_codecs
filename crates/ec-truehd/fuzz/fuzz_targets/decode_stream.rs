#![no_main]

use ec_truehd::{TrueHdDecoder, frame_length};
use libfuzzer_sys::fuzz_target;

// The whole input is one TrueHD/MLP elementary stream. Split it into access units by
// their own length words, as a demuxer would, and feed each one to a single decoder so
// stream state (restart headers, filter history) carries from one unit to the next.
fuzz_target!(|data: &[u8]| {
    let mut dec = TrueHdDecoder::new();
    let mut pos = 0;
    while pos < data.len() {
        // frame_length never returns less than 4, so `pos` always advances.
        let Ok(len) = frame_length(&data[pos..]) else {
            break;
        };
        let Some(au) = data.get(pos..pos + len) else {
            break;
        };
        let _ = dec.decode_access_unit(au);
        pos += len;
    }
});

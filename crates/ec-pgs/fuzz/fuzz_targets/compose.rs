#![no_main]

use ec_pgs::{PgsDecoder, SEG_END, SEG_ODS, SEG_PCS, SEG_PDS, SEG_WDS};
use libfuzzer_sys::fuzz_target;

// Composition from structured segments. The input is read as a list of (kind, length, body)
// records: the kind byte picks PCS, WDS, PDS, ODS or END, the length byte gives the body size, and
// the body is the next bytes of input. Each record is framed as a valid `.sup` segment, so the
// bodies (canvas size, placement, window, palette and object fields) reach the composer every time.
fuzz_target!(|data: &[u8]| {
    const KINDS: [u8; 5] = [SEG_PCS, SEG_WDS, SEG_PDS, SEG_ODS, SEG_END];
    let mut stream = Vec::new();
    let mut at = 0;
    while at + 2 <= data.len() {
        let kind = KINDS[usize::from(data[at]) % KINDS.len()];
        let len = usize::from(data[at + 1]);
        at += 2;
        let body = &data[at..(at + len).min(data.len())];
        at += body.len();
        stream.extend_from_slice(b"PG");
        stream.extend_from_slice(&[0; 8]);
        stream.push(kind);
        stream.extend_from_slice(&(body.len() as u16).to_be_bytes());
        stream.extend_from_slice(body);
    }
    let mut decoder = PgsDecoder::new();
    let _ = decoder.push(&stream);
    while let Some(frame) = decoder.take_frame() {
        std::hint::black_box((frame.width, frame.height));
    }
});

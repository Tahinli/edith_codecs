#![no_main]

// Untrusted entry: arbitrary bytes pushed into the MP1/MP2/MP3 byte-stream reader in chunks
// sized by the first byte, then every frame is decoded until the reader reports an error or
// runs dry. Errors are expected for malformed input; only panics, hangs and OOM count.

use ec_mp3::Mp3Reader;
use libfuzzer_sys::fuzz_target;

/// Harness cap: frames decoded per input. A Layer III frame is 1152 samples, so this bounds
/// the work per input without starving the reader of resync and reservoir paths.
const MAX_FRAMES: usize = 512;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else {
        return;
    };
    let chunk = 1 + usize::from(sel) * 13;
    let mut reader = Mp3Reader::new();
    let mut frames = 0;
    for piece in rest.chunks(chunk) {
        reader.push(piece);
        while let Ok(frame) = reader.next_frame() {
            std::hint::black_box(frame);
            frames += 1;
            if frames == MAX_FRAMES {
                return;
            }
        }
    }
});

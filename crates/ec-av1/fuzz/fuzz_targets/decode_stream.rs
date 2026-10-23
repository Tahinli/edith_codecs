#![no_main]

// Untrusted entry: a raw AV1 OBU stream through the full decoder
// (ec_av1::stream::decode_stream_with_threads). Errors are expected for malformed
// input; only panics, hangs and OOM count.
//
// Caps, so one iteration stays in the milliseconds and the fuzzer cannot OOM:
// - a sequence header larger than MAX_WIDTH x MAX_HEIGHT is not decoded at all;
// - at most MAX_FRAMES frame OBUs are decoded: the stream is cut before the next one;
// - the decoder runs on one thread.

use ec_av1::stream::decode_stream_with_threads;
use ec_av1_syntax::{Av1Parser, ObuKind};
use libfuzzer_sys::fuzz_target;

/// Largest frame decoded.
const MAX_WIDTH: u32 = 64;
const MAX_HEIGHT: u32 = 64;
/// Frame OBUs decoded per input.
const MAX_FRAMES: usize = 8;

/// Pre-scan the OBUs: `None` when a sequence header is over the size limit, otherwise the
/// byte length of the prefix that holds at most MAX_FRAMES frames.
fn capped_len(data: &[u8]) -> Option<usize> {
    let mut parser = Av1Parser::new();
    let mut pos = 0usize;
    let mut frames = 0usize;
    while let Some(rest) = data.get(pos..).filter(|r| !r.is_empty()) {
        let Ok(obu) = parser.parse_obu(rest) else {
            break;
        };
        if obu.total_size == 0 {
            break;
        }
        match &obu.kind {
            ObuKind::SequenceHeader(seq) => {
                if seq.max_frame_width > MAX_WIDTH || seq.max_frame_height > MAX_HEIGHT {
                    return None;
                }
            }
            ObuKind::Frame(..) | ObuKind::FrameHeader(..) => {
                frames += 1;
                if frames > MAX_FRAMES {
                    return Some(pos);
                }
            }
            _ => {}
        }
        pos += obu.total_size;
    }
    Some(data.len())
}

fuzz_target!(|data: &[u8]| {
    let Some(len) = capped_len(data) else {
        return;
    };
    let _ = decode_stream_with_threads(&data[..len], 1, |picture, _index, _shown| {
        std::hint::black_box(picture);
        Ok(())
    });
});

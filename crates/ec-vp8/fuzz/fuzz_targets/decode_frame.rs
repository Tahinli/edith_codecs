#![no_main]

use ec_vp8::decode::Decoder;
use libfuzzer_sys::fuzz_target;

/// Harness cap (see lanes/fuzz1hvideo.report.md): a key frame with luma above 128x128 is not
/// fed. Inter frames inherit the size of the key frame before them, so the input is skipped
/// when its own header is an oversized key frame.
const MAX_DIM: u32 = 128;

fuzz_target!(|data: &[u8]| {
    if oversized_key_frame(data) {
        return;
    }
    // One frame through a fresh decoder. Errors are the expected outcome for
    // malformed input; the target only looks for panics, hangs and OOM.
    let mut dec = Decoder::new();
    let _ = dec.decode(data);
});

fn oversized_key_frame(frame: &[u8]) -> bool {
    matches!(key_frame_dims(frame), Some((w, h)) if w > MAX_DIM || h > MAX_DIM)
}

/// Size of a key frame from its frame tag and start code (RFC 6386 9.1), or `None` for
/// inter frames and frames too short to hold the header.
fn key_frame_dims(frame: &[u8]) -> Option<(u32, u32)> {
    let [tag0, _, _, 0x9d, 0x01, 0x2a, w0, w1, h0, h1, ..] = *frame else {
        return None;
    };
    if tag0 & 1 != 0 {
        return None;
    }
    let width = u32::from(u16::from_le_bytes([w0, w1]) & 0x3fff);
    let height = u32::from(u16::from_le_bytes([h0, h1]) & 0x3fff);
    Some((width, height))
}

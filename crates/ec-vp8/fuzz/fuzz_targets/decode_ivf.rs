#![no_main]

use libfuzzer_sys::fuzz_target;

/// Harness caps (see lanes/fuzz1hvideo.report.md): at most 8 IVF frames per input, and the
/// stream is cut before the first key frame with luma above 128x128.
const MAX_DIM: u32 = 128;
const MAX_FRAMES: usize = 8;
const IVF_HEADER: usize = 32;
const IVF_FRAME_HEADER: usize = 12;

fuzz_target!(|data: &[u8]| {
    // Whole IVF stream through the container demuxer and the frame decoder.
    let _ = ec_vp8::stream::decode_stream(capped(data));
});

/// The prefix of `data` that holds at most `MAX_FRAMES` IVF frames and no oversized key frame.
fn capped(data: &[u8]) -> &[u8] {
    let mut end = IVF_HEADER.min(data.len());
    for _ in 0..MAX_FRAMES {
        let Some(head) = data.get(end..end + IVF_FRAME_HEADER) else {
            break;
        };
        let size = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let start = end + IVF_FRAME_HEADER;
        let stop = start.saturating_add(size).min(data.len());
        if oversized_key_frame(&data[start..stop]) {
            break;
        }
        end = stop;
    }
    &data[..end]
}

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

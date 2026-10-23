#![no_main]

// Untrusted entry: a raw Annex B byte stream fed to the NAL-level Decoder.
// Every NAL is pushed, then the stream is flushed and the output drained.
// Errors are expected for malformed input; only panics, hangs and OOM count.

use ec_h264::{Decoder, NalOutcome};
use libfuzzer_sys::fuzz_target;

/// Split an Annex B stream on 3-byte start codes, trimming the zero bytes
/// that belong to 4-byte start codes and trailing zeros.
fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (k, &start) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(data.len(), |&next| next - 3);
        while end > start && data[end - 1] == 0 {
            end -= 1;
        }
        if end > start {
            units.push(&data[start..end]);
        }
    }
    units
}

fn drain(decoder: &mut Decoder) {
    while let Some(frame) = decoder.next_frame() {
        std::hint::black_box(frame);
    }
}

/// Harness caps (see lanes/fuzz1hvideo.report.md): luma at most 128x128, at most
/// 64 NAL units per input. Oversized pictures and the NALs after the cap are not fed.
const MAX_DIM: u32 = 128;
const MAX_NALS: usize = 64;

fuzz_target!(|data: &[u8]| {
    let mut decoder = Decoder::new();
    for nal in nal_units(data).into_iter().take(MAX_NALS) {
        if let Some((w, h)) = decoder.picture_size() {
            if w > MAX_DIM || h > MAX_DIM {
                break;
            }
        }
        if let Ok(NalOutcome::PictureBoundary) = decoder.push_nal(nal) {
            // The NAL opens the next picture: close the open one, then push it again.
            let _ = decoder.end_picture();
            let _ = decoder.push_nal(nal);
        }
        drain(&mut decoder);
    }
    let _ = decoder.flush();
    drain(&mut decoder);
});

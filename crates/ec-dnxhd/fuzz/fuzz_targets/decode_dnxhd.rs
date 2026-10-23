#![no_main]

// Untrusted entry: arbitrary bytes as one DNxHD/DNxHR frame packet. Errors are
// expected for malformed input; only panics, hangs and OOM count.

use ec_core::registry::{CodecId, CodecParameters};
use ec_dnxhd::DnxhdDecoder;
use libfuzzer_sys::fuzz_target;

/// Harness cap: pictures larger than this are skipped (the decoder itself
/// bounds allocation at 8192x4608; the cap keeps fuzz runs fast).
const MAX_PIXELS: u64 = 2_000_000;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 0x1c {
        let h = u64::from(u16::from_be_bytes([data[0x18], data[0x19]]));
        let w = u64::from(u16::from_be_bytes([data[0x1a], data[0x1b]]));
        // Interlaced frames double the stated height.
        if w * h * 2 > MAX_PIXELS {
            return;
        }
    }
    let Ok(mut dec) = DnxhdDecoder::new(CodecParameters::new(CodecId::Dnxhd)) else {
        return;
    };
    dec.set_threads(1 + data.len() % 3);
    if data.first().is_some_and(|b| b & 1 == 1) {
        let _ = std::hint::black_box(dec.decode_native(data));
    } else {
        let _ = std::hint::black_box(dec.decode_bytes(data));
    }
});

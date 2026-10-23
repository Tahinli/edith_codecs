#![no_main]

// Untrusted entry: arbitrary bytes fed to the MPEG-1/2 video byte-stream API
// in random-sized chunks, then flushed and drained. Errors are expected for
// malformed input; only panics, hangs and OOM count.

use ec_core::registry::{CodecId, CodecParameters};
use ec_mpeg2::Mpeg2Decoder;
use libfuzzer_sys::fuzz_target;

/// Harness cap: pictures larger than this are not fed further (the decoder
/// itself bounds allocation at 8192x4352; the cap keeps fuzz runs fast).
const MAX_DIM: u32 = 256;

fn drain(dec: &mut Mpeg2Decoder) {
    while let Some(p) = dec.pop_picture() {
        std::hint::black_box(p);
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(mut dec) = Mpeg2Decoder::new(CodecParameters::new(CodecId::Mpeg2Video)) else {
        return;
    };
    // Chunk sizes come from a xorshift seeded by the input length and head bytes.
    let mut s = 0x9E37_79B9_7F4A_7C15u64 ^ data.len() as u64;
    for &b in data.iter().take(8) {
        s = s.rotate_left(8) ^ u64::from(b);
    }
    let mut rest = data;
    while !rest.is_empty() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let n = match s % 4 {
            0 => 1 + (s >> 8) as usize % 8,
            1 => 1 + (s >> 8) as usize % 64,
            2 => 1 + (s >> 8) as usize % 1024,
            _ => rest.len(),
        }
        .min(rest.len());
        let (chunk, tail) = rest.split_at(n);
        rest = tail;
        let _ = dec.send_bytes(chunk, None);
        drain(&mut dec);
        if let Some(seq) = dec.sequence() {
            if seq.width > MAX_DIM || seq.height > MAX_DIM {
                break;
            }
        }
    }
    let _ = dec.flush_stream();
    drain(&mut dec);
});

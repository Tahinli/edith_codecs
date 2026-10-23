#![no_main]

// Untrusted entry: arbitrary bytes fed to the MPEG-4 Part 2 / H.263 byte-stream API in
// random-sized chunks, then flushed and drained. Errors are expected for malformed input;
// only panics, hangs and OOM count. The first input byte picks the FourCC-less default,
// DivX or XviD IDCT forcing, so both IDCT paths are reached.

use ec_core::registry::{CodecId, CodecParameters};
use ec_mpeg4::Mpeg4Decoder;
use libfuzzer_sys::fuzz_target;

fn drain(dec: &mut Mpeg4Decoder) {
    while let Some(p) = dec.pop_picture() {
        std::hint::black_box(p);
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(mut dec) = Mpeg4Decoder::new(CodecParameters::new(CodecId::Mpeg4Part2)) else {
        return;
    };
    let Some((&mode, data)) = data.split_first() else {
        return;
    };
    match mode & 3 {
        1 => dec.set_xvid_idct(Some(true)),
        2 => dec.set_xvid_idct(Some(false)),
        _ => {}
    }
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
    }
    let _ = dec.flush_stream();
    drain(&mut dec);
});

#![no_main]

use ec_inflate::{inflate, inflate_zlib};
use libfuzzer_sys::fuzz_target;

const LIMIT: usize = 16 << 20;

// Differential: byte 0 bit 0 picks raw or zlib. Whenever both decoders succeed,
// the bytes must match. Either side may reject input the other accepts; that is not a finding here.
fuzz_target!(|data: &[u8]| {
    let Some((&sel, body)) = data.split_first() else {
        return;
    };
    let (ours, theirs) = if sel & 1 == 0 {
        (
            inflate(body, LIMIT),
            miniz_oxide::inflate::decompress_to_vec_with_limit(body, LIMIT),
        )
    } else {
        (
            inflate_zlib(body, LIMIT),
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(body, LIMIT),
        )
    };
    if let (Ok(a), Ok(b)) = (ours, theirs) {
        assert_eq!(a, b, "ec-inflate and miniz_oxide disagree");
    }
});

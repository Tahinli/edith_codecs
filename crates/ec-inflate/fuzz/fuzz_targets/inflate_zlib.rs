#![no_main]

use ec_inflate::inflate_zlib;
use libfuzzer_sys::fuzz_target;

// Whole input is one zlib stream (header, DEFLATE blocks, Adler-32), capped at 16 MiB.
fuzz_target!(|data: &[u8]| {
    let _ = inflate_zlib(data, 16 << 20);
});

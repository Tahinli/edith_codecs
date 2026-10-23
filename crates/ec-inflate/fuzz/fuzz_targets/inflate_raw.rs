#![no_main]

use ec_inflate::inflate;
use libfuzzer_sys::fuzz_target;

// Whole input is one raw DEFLATE stream, capped at 16 MiB of output.
fuzz_target!(|data: &[u8]| {
    let _ = inflate(data, 16 << 20);
});

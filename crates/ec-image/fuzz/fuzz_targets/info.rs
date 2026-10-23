#![no_main]

use libfuzzer_sys::fuzz_target;

// Header-only parse: dimensions and format details without decoding any pixels.
fuzz_target!(|data: &[u8]| {
    let _ = ec_image::info(data);
});

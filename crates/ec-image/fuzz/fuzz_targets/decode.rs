#![no_main]

use libfuzzer_sys::fuzz_target;

// The public entry point: the format is guessed from the leading bytes, then the whole picture is
// decoded under the default limits.
fuzz_target!(|data: &[u8]| {
    let _ = ec_image::decode(data);
});

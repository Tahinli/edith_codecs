#![no_main]

use ec_matroska::fuzz_api;
use libfuzzer_sys::fuzz_target;

// The EBML grammar on its own: element headers (id and vint size, both widths),
// the unknown-length marker, and every nested master read as integers, floats and
// strings. This is what every Matroska parser above it stands on.
fuzz_target!(|data: &[u8]| {
    let _ = fuzz_api::ebml_walk(data);
});

#![no_main]

use ec_flac::FlacReader;
use libfuzzer_sys::fuzz_target;

// The whole input is one .flac buffer: magic, metadata, then every frame.
fuzz_target!(|data: &[u8]| {
    if let Ok(mut reader) = FlacReader::new(data) {
        let _ = reader.decode_all();
    }
});

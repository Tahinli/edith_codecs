#![no_main]

// The whole input is one RIFF/AVI file. Headers are parsed by `new`, then every audio
// packet is pulled until the reader reports an error (end of stream or corruption).

use std::io::Cursor;

use ec_riff::AviReader;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(mut reader) = AviReader::new(Cursor::new(data)) else {
        return;
    };
    let _ = reader.audio_streams().len();
    let _ = reader.has_index();
    let _ = reader.index_len();
    while reader.next_packet().is_ok() {}
});

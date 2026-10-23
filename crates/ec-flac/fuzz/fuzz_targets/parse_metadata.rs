#![no_main]

use ec_flac::{FlacReader, StreamInfo};
use libfuzzer_sys::fuzz_target;

// Metadata only: the STREAMINFO payload on its own, and the whole stream opened
// as a reader (magic, metadata blocks, seek table), with a seek on the result.
fuzz_target!(|data: &[u8]| {
    if let Ok(info) = StreamInfo::parse(data) {
        // Every field is a bit-exact slice of the 34 bytes, so packing back must
        // reproduce them.
        assert_eq!(info.to_bytes()[..], data[..34]);
    }
    if let Ok(mut reader) = FlacReader::new(data) {
        let _ = reader.stream_info();
        let _ = reader.seek_table();
        let _ = reader.seek_to_sample(0);
        let _ = reader.seek_to_sample(u64::MAX);
    }
});

#![no_main]

use ec_dts::{DtsDecoder, find_sync};
use libfuzzer_sys::fuzz_target;

// The whole input is a raw stream: decode every frame found by sync search, the way a
// container would hand over packets. A failed frame advances by one byte.
fuzz_target!(|data: &[u8]| {
    let mut dec = DtsDecoder::new();
    let mut pos = 0;
    while pos < data.len() {
        let Some((off, _)) = find_sync(&data[pos..]) else { break };
        let start = pos + off;
        let rest = &data[start..];
        let size = match ec_dts::parse_frame_header(rest) {
            Ok(h) if h.stream_frame_size <= rest.len() && h.stream_frame_size > 0 => h.stream_frame_size,
            _ => {
                pos = start + 1;
                continue;
            }
        };
        let _ = dec.decode_frame(&rest[..size]);
        pos = start + size;
    }
});

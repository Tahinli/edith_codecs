#![no_main]

// Input layout:
//   byte 0      bit 0 = `after` flag for the seek
//   byte 1      stream number (mod 100)
//   bytes 2..10 seek target, u64 little-endian, zero-padded when short
//   then        the RIFF/AVI file
// The seek is followed by a full packet drain, so both seek paths and the reads that
// follow them are exercised.

use std::io::Cursor;

use ec_riff::AviReader;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 10 {
        return;
    }
    let after = data[0] & 1 == 1;
    let stream = u32::from(data[1] % 100);
    let mut target = [0u8; 8];
    target.copy_from_slice(&data[2..10]);
    let target = u64::from_le_bytes(target);
    let file = &data[10..];

    let Ok(mut reader) = AviReader::new(Cursor::new(file)) else {
        return;
    };
    let _ = reader.indexed_point(stream, target, after);
    let _ = reader.seek_to_stream_time(stream, target, after);
    while reader.next_packet().is_ok() {}
    reader.rewind();
    while reader.next_packet().is_ok() {}
});

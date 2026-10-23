#![no_main]

// Input layout:
//   byte 0      stream number (mod 4)
//   byte 1      seek mode (mod 3)
//   bytes 2..6  seek target in ticks, i32 little-endian
//   then        the RIFF/AVI file
// Opens the full A/V demuxer, drains it, seeks, and drains again.

use std::io::Cursor;

use ec_core::registry::{Demuxer, SeekMode};
use ec_core::{TimeBase, Timestamp};
use ec_riff::AviDemuxer;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 6 {
        return;
    }
    let stream = u32::from(data[0] % 4);
    let mode = match data[1] % 3 {
        0 => SeekMode::SyncBefore,
        1 => SeekMode::SyncAfter,
        _ => SeekMode::Exact,
    };
    let target = i32::from_le_bytes([data[2], data[3], data[4], data[5]]);
    let Ok(mut d) = AviDemuxer::new(Cursor::new(&data[6..])) else {
        return;
    };
    let _ = d.field_order(stream);
    let _ = d.tracks();
    while d.next_packet().is_ok() {}
    let _ = d.seek(stream, Timestamp::new(i64::from(target), TimeBase::new(1, 25)), mode);
    while d.next_packet().is_ok() {}
});

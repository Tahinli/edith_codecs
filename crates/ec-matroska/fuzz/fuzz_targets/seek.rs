#![no_main]

use std::io::Cursor;

use ec_core::{Demuxer, SeekMode, Timestamp};
use ec_matroska::MatroskaDemuxer;
use libfuzzer_sys::fuzz_target;

// The last 9 bytes are a seek request: one byte picks the stream and the mode, eight
// give the target tick count. Everything before them is the file. A seek builds the
// Cues / SeekHead / cluster index, then packets are read from where it landed.
fuzz_target!(|data: &[u8]| {
    let Some((file, req)) = data.len().checked_sub(9).map(|at| data.split_at(at)) else {
        return;
    };
    let Ok(mut dmx) = MatroskaDemuxer::new(Cursor::new(file)) else {
        return;
    };
    let streams = dmx.streams().len();
    if streams == 0 {
        return;
    }
    let stream = u32::from(req[0] >> 2) % streams as u32;
    let mode = match req[0] & 0x3 {
        0 => SeekMode::SyncBefore,
        1 => SeekMode::SyncAfter,
        _ => SeekMode::Exact,
    };
    let ticks = i64::from_le_bytes([req[1], req[2], req[3], req[4], req[5], req[6], req[7], req[8]]);
    let to = Timestamp::new(ticks, dmx.time_base());
    if dmx.seek(stream, to, mode).is_err() {
        return;
    }
    for _ in 0..64 {
        match dmx.next_packet() {
            Ok(packet) => {
                let _ = packet.data.len();
            }
            Err(_) => break,
        }
    }
});

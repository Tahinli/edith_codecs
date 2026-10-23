#![no_main]

use std::io::Cursor;

use ec_core::registry::{Demuxer, SeekMode};
use ec_core::{TimeBase, Timestamp};
use ec_mpegts::{TsDemuxer, VideoFraming};
use libfuzzer_sys::fuzz_target;

// The whole input is one file: open it (layout, PAT, PMT, probe, tail scan), list
// the tracks, read packets until the end or an error, then seek to a few targets in
// every mode and read again. With length-prefixed video on, too.
fuzz_target!(|data: &[u8]| {
    let Ok(mut demux) = TsDemuxer::new(Cursor::new(data)) else {
        return;
    };
    let _ = (demux.tracks().len(), demux.programs().len(), demux.duration_secs());
    // The first pass reads in Annex-B, the second (after a seek) length-prefixed.
    for round in 0..2 {
        if round == 1 {
            demux.set_video_framing(VideoFraming::LengthPrefixed);
        }
        for _ in 0..2_000 {
            if demux.next_packet().is_err() {
                break;
            }
        }
        let streams = demux.streams().len() as u32;
        for stream in 0..streams.min(2) {
            for (i, ticks) in [0i64, 90_000 * (round + 1), 1 << 33, -5].into_iter().enumerate() {
                let mode = [SeekMode::SyncBefore, SeekMode::SyncAfter, SeekMode::Exact][(i + stream as usize) % 3];
                let _ = demux.seek(stream, Timestamp::new(ticks, TimeBase::new(1, 90_000)), mode);
                for _ in 0..20 {
                    if demux.next_packet().is_err() {
                        break;
                    }
                }
            }
        }
    }
});

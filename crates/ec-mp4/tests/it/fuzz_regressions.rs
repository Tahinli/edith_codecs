//! Replays of inputs that the fuzzer found to crash the MP4 demuxer. Each input is replayed
//! in full, so a regression shows up here even if the fuzz harness would stop early.

use std::io::Cursor;

use ec_core::registry::Demuxer;
use ec_mp4::Mp4Demuxer;

/// `demux_mp4` panic: `frame_rate` summed `count * delta` over the stts entries in u64 with
/// plain `+`, so two large entries overflowed (demux.rs, the `ticks` fold). The sums now
/// saturate; the file must open or be refused, and every sample must read or error, never panic.
#[test]
fn stts_tick_sum_overflow_is_refused() {
    let data = include_bytes!("../data/fuzz/demux_mp4-stts-tick-overflow.mp4");
    let Ok(mut demux) = Mp4Demuxer::new(Cursor::new(&data[..])) else {
        return;
    };
    let _ = demux.tracks().len();
    let _ = demux.duration_secs();
    while let Ok(_packet) = demux.next_packet() {}
}

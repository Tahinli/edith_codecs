#![no_main]

use std::io::Cursor;

use ec_core::registry::Demuxer;
use ec_mp4::Mp4Demuxer;
use libfuzzer_sys::fuzz_target;

// The whole input is one file: open it (moov, sample tables, fragments), list the
// tracks, read every sample of every track in storage order until the end or an
// error, then read every sample again by index with `read_sample`.
fuzz_target!(|data: &[u8]| {
    let Ok(mut demux) = Mp4Demuxer::new(Cursor::new(data)) else {
        return;
    };
    let _ = demux.tracks().len();
    let _ = demux.duration_secs();
    let _ = demux.fragment_count();
    loop {
        match demux.next_packet() {
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let streams = demux.streams().len() as u32;
    for stream in 0..streams {
        let count = demux.samples(stream).len();
        for index in 0..count {
            if demux.read_sample(stream, index).is_err() {
                break;
            }
        }
    }
});

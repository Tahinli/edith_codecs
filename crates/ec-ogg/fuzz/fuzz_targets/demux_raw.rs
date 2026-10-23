#![no_main]

use std::io::Cursor;

use ec_core::Demuxer;
use ec_ogg::OggDemuxer;
use libfuzzer_sys::fuzz_target;

// The input as-is: pages with bad checksums are dropped by the reader, so this target covers
// the CRC gate itself and whatever survives it.
fuzz_target!(|data: &[u8]| {
    let Ok(mut demux) = OggDemuxer::open(Cursor::new(data)) else {
        return;
    };
    while demux.next_packet().is_ok() {}
});

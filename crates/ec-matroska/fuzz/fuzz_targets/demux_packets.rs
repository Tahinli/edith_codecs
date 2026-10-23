#![no_main]

use std::io::Cursor;

use ec_core::Demuxer;
use ec_matroska::MatroskaDemuxer;
use libfuzzer_sys::fuzz_target;

// Opens the input and reads every packet until the end or the first error. This walks
// clusters, SimpleBlock and BlockGroup, every lacing shape, ContentEncodings and the
// resynchronisation path over damaged clusters.
fuzz_target!(|data: &[u8]| {
    let Ok(mut dmx) = MatroskaDemuxer::new(Cursor::new(data)) else {
        return;
    };
    for (i, stream) in dmx.streams().iter().enumerate() {
        let _ = dmx.track_codec_id(i as u32);
        let _ = stream.params.extradata.as_ref().map(|e| e.len());
    }
    while let Ok(packet) = dmx.next_packet() {
        let _ = packet.data.len();
    }
});

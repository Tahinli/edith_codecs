#![no_main]

use std::io::Cursor;

use ec_core::Demuxer;
use ec_matroska::MatroskaDemuxer;
use libfuzzer_sys::fuzz_target;

// Opening walks the EBML header, Segment, Info, SeekHead and Tracks. Every track's
// codec id, private data and accessors are read back, as a caller listing tracks would.
fuzz_target!(|data: &[u8]| {
    let Ok(dmx) = MatroskaDemuxer::new(Cursor::new(data)) else {
        return;
    };
    let _ = dmx.doc_type();
    let _ = dmx.time_base();
    for (i, stream) in dmx.streams().iter().enumerate() {
        let i = i as u32;
        let _ = dmx.track_number(i);
        let _ = dmx.track_codec_id(i);
        let _ = dmx.track_title(i);
        let _ = dmx.track_refusal(i);
        let _ = dmx.track_is_plain(i);
        let _ = dmx.color_tags(i);
        let _ = stream.params.extradata.as_ref().map(|e| e.len());
    }
    let _ = dmx.unread_tracks();
});

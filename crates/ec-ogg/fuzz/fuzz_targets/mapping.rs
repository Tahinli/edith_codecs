#![no_main]

use ec_ogg::{Mapping, opus_packet_samples};
use libfuzzer_sys::fuzz_target;

// The first packet of a stream is identified, then every accessor the demuxer reads from it is
// called. The same bytes are also read as an Opus packet, whose TOC byte sets its duration.
fuzz_target!(|data: &[u8]| {
    let _ = opus_packet_samples(data);
    if let Some(mapping) = Mapping::identify(data) {
        let _ = mapping.codec();
        let _ = mapping.time_base();
        let _ = mapping.sample_rate();
        let _ = mapping.layout();
        let _ = mapping.header_packets();
        let _ = mapping.packet_duration(data);
    }
});

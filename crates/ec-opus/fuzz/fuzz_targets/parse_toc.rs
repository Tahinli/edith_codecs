#![no_main]

// The whole input is one Opus packet: its first byte is the TOC, the rest the frame data.
// Exercises the TOC accessors and both packet framings (plain and self-delimited).

use ec_opus::{Packet, Toc};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some(&toc_byte) = data.first() else {
        return;
    };
    let toc = Toc::new(toc_byte);
    let _ = (toc.mode(), toc.bandwidth(), toc.frame_size_48k(), toc.channels());
    if let Ok(p) = Packet::parse(data, false) {
        let _ = p.samples_48k();
    }
    if let Ok(p) = Packet::parse(data, true) {
        let _ = p.samples_48k();
    }
});

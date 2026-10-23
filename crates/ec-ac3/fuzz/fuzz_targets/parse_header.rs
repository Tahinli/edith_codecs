#![no_main]

use ec_ac3::{bsi, eac3, syncinfo};
use libfuzzer_sys::fuzz_target;

// Header-only paths: syncinfo + BSI for AC-3, and the E-AC-3 BSI, plus the frame-length probe.
fuzz_target!(|data: &[u8]| {
    let _ = syncinfo::parse(data);
    if data.len() > 5 {
        let _ = bsi::parse(&data[5..]);
    }
    let _ = eac3::bsi::parse(data);
    let _ = ec_ac3::frame_size(data);
});

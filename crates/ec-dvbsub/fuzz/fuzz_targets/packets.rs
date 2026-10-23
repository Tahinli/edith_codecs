#![no_main]

use ec_dvbsub::DvbSubDecoder;
use libfuzzer_sys::fuzz_target;

// Arbitrary packets, in two pieces so state carries over.
fuzz_target!(|data: &[u8]| {
    let mut d = DvbSubDecoder::new();
    let cut = data.len() / 2;
    let _ = d.push_pes(&data[..cut]);
    let _ = d.push_pes(&data[cut..]);
    while d.take_cue().is_some() {}
});

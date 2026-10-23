#![no_main]

use ec_matroska::fuzz_api;
use libfuzzer_sys::fuzz_target;

// Block lacing: the first byte picks Xiph (1), fixed (2) or EBML (3) lacing and the
// rest is the lace header and frames. The whole input is also read as a complete
// block (track number, timecode, flags, lace), which is the form the demuxer sees.
fuzz_target!(|data: &[u8]| {
    let Some((&first, rest)) = data.split_first() else {
        return;
    };
    let _ = fuzz_api::lace(rest, 1 + first % 3);
    let _ = fuzz_api::block(data);
});

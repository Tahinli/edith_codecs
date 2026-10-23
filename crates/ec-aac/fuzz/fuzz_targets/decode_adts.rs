#![no_main]

use ec_aac::AacDecoder;
use libfuzzer_sys::fuzz_target;

// The whole input is one packet of ADTS frames: `decode` walks every frame in it.
fuzz_target!(|data: &[u8]| {
    let mut dec = AacDecoder::new();
    let _ = dec.decode(data, None);
});

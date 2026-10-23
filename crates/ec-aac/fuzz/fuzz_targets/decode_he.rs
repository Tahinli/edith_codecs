#![no_main]

use ec_aac::AacDecoder;
use libfuzzer_sys::fuzz_target;

// HE-AAC path: the whole input is ADTS, so every frame goes through the SBR (and, for v2, PS)
// reconstruction. Seeds are the HE-AAC v1/v2 fixtures in tests/data/he (see seed.sh).
fuzz_target!(|data: &[u8]| {
    let mut dec = AacDecoder::new();
    let _ = dec.decode(data, None);
});

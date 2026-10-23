#![no_main]

use ec_mp4::Esds;
use libfuzzer_sys::fuzz_target;

// The whole input is the payload of an `esds` box, after its version/flags word:
// the ES, DecoderConfig and DecoderSpecificInfo descriptors.
fuzz_target!(|data: &[u8]| {
    if let Ok(esds) = Esds::parse(data) {
        let _ = esds.is_aac();
        let _ = esds.decoder_specific.as_ref().map(|c| c.len());
    }
});

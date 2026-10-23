#![no_main]

use ec_amr::container::nb_frame_size;
use ec_amr::nb::decoder::AmrNbDecoder;
use ec_core::registry::{CodecId, CodecParameters};
use libfuzzer_sys::fuzz_target;

// The input is a run of ToC byte + payload frames. Each is cut at the length
// its ToC states (a short tail is passed as-is) and fed to one decoder, so
// state carries across frames the way it does in a file.
fuzz_target!(|data: &[u8]| {
    let Ok(mut dec) = AmrNbDecoder::from_parameters(CodecParameters::new(CodecId::AmrNb)) else {
        return;
    };
    let mut rest = data;
    while let Some(&toc) = rest.first() {
        let size = nb_frame_size((toc >> 3) & 0x0F).unwrap_or(0);
        let n = (1 + size).min(rest.len());
        let _ = dec.decode(&rest[..n]);
        rest = &rest[n..];
    }
});

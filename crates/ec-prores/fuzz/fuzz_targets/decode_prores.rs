#![no_main]

// Untrusted entry: arbitrary bytes as one ProRes frame (and as a packet through
// the registry contract). Errors are expected; only panics, hangs and OOM count.

use ec_core::packet::Packet;
use ec_core::registry::{CodecId, CodecParameters, Decoder};
use ec_prores::ProResDecoder;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut dec = ProResDecoder::new(CodecParameters::new(CodecId::ProRes)).unwrap();
    dec.set_threads(1 + data.len() % 3);
    if let Ok(f) = dec.decode(data) {
        std::hint::black_box(f);
    }
    let _ = dec.send_packet(&Packet::new(0, ec_core::timebase::TimeBase::from_rate(25), data.to_vec()));
    while dec.receive_frame().is_ok() {}
});

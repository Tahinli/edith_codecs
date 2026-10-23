#![no_main]

use ec_amr::wb::AmrWbDecoder;
use libfuzzer_sys::fuzz_target;

// Arbitrary bytes as AMR-WB packets (ToC byte then speech bits, one or more frames each). The
// input is cut into packets whose length its first byte picks, so the decoder sees both long
// multi-frame packets and many short ones; one decoder carries its state across all of them. Errors are fine, panics and hangs are not.
fuzz_target!(|data: &[u8]| {
    let Some((&cut, rest)) = data.split_first() else {
        return;
    };
    let mut dec = AmrWbDecoder::new();
    let chunk = 1 + usize::from(cut) % 80;
    for packet in rest.chunks(chunk) {
        let _ = dec.decode(packet);
    }
    let _ = dec.decode(rest);
});

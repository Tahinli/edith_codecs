#![no_main]

use ec_pgs::PgsDecoder;
use libfuzzer_sys::fuzz_target;

// Raw `.sup` bytes, split in two at a point the first byte picks, so a segment torn across two
// `push` calls is reached. Every frame that comes out is drained as the stream goes.
fuzz_target!(|data: &[u8]| {
    let Some((&cut, _)) = data.split_first() else {
        return;
    };
    let split = usize::from(cut) % (data.len() + 1);
    let mut decoder = PgsDecoder::new();
    for part in [&data[..split], &data[split..]] {
        let _ = decoder.push(part);
        while let Some(frame) = decoder.take_frame() {
            std::hint::black_box((frame.width, frame.height));
        }
    }
});

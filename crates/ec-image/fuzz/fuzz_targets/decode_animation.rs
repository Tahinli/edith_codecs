#![no_main]

use libfuzzer_sys::fuzz_target;

// Every frame of a GIF or animated WebP, composited onto the canvas; anything else decodes as one
// frame.
fuzz_target!(|data: &[u8]| {
    let _ = ec_image::decode_animation(data);
});

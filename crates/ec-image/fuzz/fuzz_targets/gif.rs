#![no_main]

use ec_image::Limits;
use libfuzzer_sys::fuzz_target;

// The gif module's own entry points, called directly so the format signature check in the
// dispatcher does not filter the input first. Header parse, then a decode under small limits.
const LIMITS: Limits = Limits {
    max_pixels: 4096 * 4096,
    max_alloc: 1 << 26,
    max_total_alloc: 1 << 27,
};

fuzz_target!(|data: &[u8]| {
    let _ = ec_image::gif::info(data);
    let _ = ec_image::gif::decode(data, LIMITS);
    let _ = ec_image::gif::decode_animation(data, LIMITS);
});

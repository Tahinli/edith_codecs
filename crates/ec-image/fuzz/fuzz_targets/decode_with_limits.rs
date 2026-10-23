#![no_main]

use ec_image::Limits;
use libfuzzer_sys::fuzz_target;

// Small limits: 4096x4096 pixels, 64 MiB per buffer, 128 MiB across one call. Under these a
// hostile header is refused before it sizes anything, and every path that multiplies a count by
// a size still gets exercised against a real budget.
const LIMITS: Limits = Limits {
    max_pixels: 4096 * 4096,
    max_alloc: 1 << 26,
    max_total_alloc: 1 << 27,
};

fuzz_target!(|data: &[u8]| {
    let _ = ec_image::decode_with_limits(data, LIMITS);
});

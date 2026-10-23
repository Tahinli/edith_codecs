#![no_main]

use ec_core::frame::Interlace;
use ec_image::Limits;
use libfuzzer_sys::fuzz_target;

const LIMITS: Limits = Limits {
    max_pixels: 4096 * 4096,
    max_alloc: 1 << 26,
    max_total_alloc: 1 << 27,
};

// The first input byte steers the container height (0 = unknown, otherwise a multiple of the
// field height) and the field order, so both the single-image and two-field paths are reached.
fuzz_target!(|data: &[u8]| {
    let Some((&steer, rest)) = data.split_first() else {
        return;
    };
    let height = u32::from(steer & 0x0f) * 16;
    let order = if steer & 0x10 != 0 {
        Interlace::BottomFieldFirst
    } else {
        Interlace::TopFieldFirst
    };
    if let Ok(f) = ec_mjpeg::decode_frame(rest, height, order, LIMITS) {
        // Every accepted frame must be well formed: try_new validated plane sizes.
        assert!(f.color.full_range);
    }
    let _ = ec_image::jpeg::decode_yuv(rest, LIMITS);
});

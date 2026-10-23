#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ec_dts::parse_frame_header(data);
    let _ = ec_dts::find_sync(data);
});

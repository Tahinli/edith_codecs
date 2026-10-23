#![no_main]

use ec_dvdsub::sub_file_units;
use libfuzzer_sys::fuzz_target;

// A `.sub` program stream of arbitrary bytes.
fuzz_target!(|data: &[u8]| {
    let _ = sub_file_units(data, 0);
});

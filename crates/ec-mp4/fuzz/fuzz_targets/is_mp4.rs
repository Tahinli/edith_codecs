#![no_main]

use libfuzzer_sys::fuzz_target;

// The container sniffer: the first bytes of any file, no parsing beyond the header.
fuzz_target!(|data: &[u8]| {
    let _ = ec_mp4::is_mp4(data);
});

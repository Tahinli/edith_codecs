#![no_main]

// The VP9 VA-API parameter builders, fed by one chunk (a frame or superframe), parsed by the
// crate's real syntax parser. Those headers are untrusted input. No GPU is needed.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ec_hw::dec::vp9_fuzz_build_parameters(data);
});

#![no_main]

// The H.264 VA-API parameter builders, fed by Annex B byte stream: SPS, PPS and slice headers, parsed by the
// crate's real syntax parser. Those headers are untrusted input. No GPU is needed.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ec_hw::dec::h264_fuzz_build_parameters(data);
});

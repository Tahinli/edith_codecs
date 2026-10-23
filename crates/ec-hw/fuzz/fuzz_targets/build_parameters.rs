#![no_main]

// The HEVC VA-API parameter builders, fed by the SPS, PPS and slice headers of an
// Annex B stream. Those headers are untrusted input. No GPU is needed.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ec_hw::dec::hevc_fuzz_build_parameters(data);
});

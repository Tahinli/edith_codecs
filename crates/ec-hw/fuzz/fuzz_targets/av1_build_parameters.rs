#![no_main]

// The AV1 VA-API parameter builders, fed by one temporal unit (sequence, frame
// and tile OBUs) parsed by the
// crate's real syntax parser. Those headers are untrusted input. No GPU is needed.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = ec_hw::dec::av1_fuzz_build_parameters(data);
});
